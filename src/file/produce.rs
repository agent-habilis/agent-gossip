//! The file producer: bind an endpoint, print the receiver's `ahsw file get`
//! command on stdout, then serve each peer only the files it is missing or
//! holds an outdated copy of.

use std::cmp::min;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use iroh::Endpoint;
use iroh::endpoint::Incoming;
use rand::RngCore;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::directory::ticket::TicketAd;
use crate::lookup::build_endpoint;
use crate::util::progress::{pace, throttle_chunk};
use crate::protocol::crypto::{Password, TicketAuth, ct_eq};
use crate::protocol::swarm::{
    DirectorySelection, LookupOpts, LookupSet, resolve_transfer_lookups, validate_advertise,
};

use super::manifest::{Entry, Manifest};
use super::ticket::FileTicket;
use super::walk::{ensure_readable, scan};
use super::wire::read_u32;
use super::{FILE_ALPN, MAX_MANIFEST_BYTES, RootKind, SECRET_LEN, wait_online};

/// Send `path` (a file or directory) to peers. Prints the receiver's
/// `ahsw file get 🐝…` command on stdout; keeps serving until interrupted,
/// re-reading the source per connection so a repeat `get` re-syncs. The
/// discovery config comes from `swarm` or the create-style `flags`;
/// `advertise` re-broadcasts the ticket into a directory so a peer can find
/// it with `ahsw file discover`.
///
/// # Errors
/// The path is unreadable, discovery-config resolution / endpoint bind
/// fails, or `--advertise` names an unreachable config.
pub(crate) async fn send(
    swarm: Option<&str>,
    flags: LookupSet,
    advertise: DirectorySelection,
    path: &Path,
    throttle: Option<u64>,
    json: bool,
    password: Option<Password>,
) -> Result<()> {
    // Fail fast if the path can't be served, before binding anything — a cheap
    // metadata check, NOT the full hashing scan (that runs per connection).
    ensure_readable(path).with_context(|| format!("cannot serve {}", path.display()))?;
    let lookups = resolve_transfer_lookups(swarm, flags)?;
    validate_advertise(&advertise, &lookups)?;
    let (endpoint, ticket, auth) = bind(lookups.clone(), password.as_ref()).await?;
    let _advertiser = match advertise.directory() {
        Some(directory) => {
            let label = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned);
            let ad = TicketAd {
                ticket: ticket.encode(),
                label,
            };
            if !json {
                crate::util::output::status_out(
                    "Advertising",
                    &format!("in #{directory} directory"),
                );
            }
            Some(crate::embed::spawn_ticket_advertiser(
                directory, lookups, &ad,
            )?)
        }
        None => None,
    };
    super::announce(
        json,
        &path.display().to_string(),
        &format!("ahsw file get {}", ticket.encode()),
    );
    let root = path.to_path_buf();
    let narrate = !json;
    while let Some(incoming) = endpoint.accept().await {
        let root = root.clone();
        let auth = auth.clone();
        tokio::spawn(async move {
            if let Err(error) = serve_connection(incoming, &auth, &root, throttle, narrate).await {
                tracing::debug!(%error, "file transfer connection ended");
            }
        });
    }
    endpoint.close().await;
    Ok(())
}

/// Bind the producer endpoint and mint its ticket + the auth token the
/// handshake expects (the raw secret, or its Argon2id stretch when
/// passworded) — no I/O, no print.
pub(super) async fn bind(
    lookups: LookupOpts,
    password: Option<&Password>,
) -> Result<(Endpoint, FileTicket, TicketAuth)> {
    let endpoint = build_endpoint(&lookups, None, None, vec![FILE_ALPN.to_vec()]).await?;
    // Loopback needs no online wait (the bound addr is immediately usable).
    if !lookups.is_loopback() {
        wait_online(&endpoint).await;
    }
    let mut secret = [0u8; SECRET_LEN];
    rand::rng().fill_bytes(&mut secret);
    let auth = TicketAuth::derive(&secret, password);
    let ticket = FileTicket {
        addr: endpoint.addr(),
        secret,
        lookups,
        password: auth.password_protected,
    };
    Ok((endpoint, ticket, auth))
}

/// Accept one connection, verify the auth token (the raw bearer secret, or
/// its password stretch), run the transfer, and close. A bad token is closed
/// with code 1 ("bad secret") — or code 3 ("wrong password") on a passworded
/// ticket, so the consumer can tell a typo from a corrupt ticket.
pub(super) async fn serve_connection(
    incoming: Incoming,
    auth: &TicketAuth,
    root: &Path,
    throttle: Option<u64>,
    narrate: bool,
) -> Result<()> {
    let conn = incoming.await.context("incoming connection failed")?;
    let (mut send, mut recv) = conn.accept_bi().await.context("accept_bi failed")?;
    // The consumer opens the bi-stream and writes its auth token first.
    let mut got = [0u8; SECRET_LEN];
    if recv.read_exact(&mut got).await.is_err() || !ct_eq(&got, &auth.token) {
        if auth.password_protected {
            conn.close(3u32.into(), b"wrong password");
            bail!("peer presented a wrong password");
        }
        conn.close(1u32.into(), b"bad secret");
        bail!("peer presented a bad secret");
    }
    serve(&mut send, &mut recv, root, throttle, narrate).await?;
    // `finish` only marks the stream done; wait (briefly) for the consumer's ACK
    // so a fast/loopback connection doesn't race CONNECTION_CLOSE ahead of the
    // last body bytes. Then close cleanly.
    let _ = send.finish();
    let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
    conn.close(0u32.into(), b"done");
    Ok(())
}

/// The producer half of the protocol: announce the tree, read the consumer's
/// manifest, and stream the diff. Generic over the streams so it is unit- and
/// loopback-testable.
///
/// Wire order (after the handshake): `kind(1) ‖ name_len(u16) ‖ name`, then the
/// consumer's `manifest_len(u32) ‖ manifest`, then the plan
/// `send_count(u32) ‖ unchanged(u32) ‖ total_bytes(u64)`, then one body per file.
pub(super) async fn serve<W, R>(
    send: &mut W,
    recv: &mut R,
    root: &Path,
    throttle: Option<u64>,
    narrate: bool,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    let scan = scan(root)?;
    let kind = scan.kind;
    super::report(narrate, "Connected", "");

    // 1. Announce what the consumer is about to receive.
    let kind_byte: u8 = match kind {
        RootKind::File => 0,
        RootKind::Dir => 1,
    };
    let name_bytes = scan.name.as_bytes();
    let name_len = u16::try_from(name_bytes.len()).context("root name too long")?;
    send.write_all(&[kind_byte]).await?;
    send.write_all(&name_len.to_le_bytes()).await?;
    send.write_all(name_bytes).await?;

    // 2. Read the consumer's manifest of what it already has.
    let manifest_len = read_u32(recv).await? as usize;
    if manifest_len > MAX_MANIFEST_BYTES {
        bail!("peer manifest too large ({manifest_len} bytes)");
    }
    let mut manifest_bytes = vec![0u8; manifest_len];
    recv.read_exact(&mut manifest_bytes).await?;
    let theirs = Manifest::decode(&manifest_bytes)?;

    // 3. Diff and send the plan.
    let to_send = scan.manifest.diff(&theirs);
    let unchanged = scan.manifest.entries.len() - to_send.len();
    let total_bytes: u64 = to_send.iter().map(|entry| entry.size).sum();
    super::report(
        narrate,
        "Sending",
        &format!(
            "{}, {unchanged} unchanged",
            super::count_files(to_send.len())
        ),
    );
    let send_count = u32::try_from(to_send.len()).unwrap_or(u32::MAX);
    let dir_count = u32::try_from(scan.empty_dirs.len()).unwrap_or(u32::MAX);
    send.write_all(&send_count.to_le_bytes()).await?;
    send.write_all(&u32::try_from(unchanged).unwrap_or(u32::MAX).to_le_bytes())
        .await?;
    send.write_all(&total_bytes.to_le_bytes()).await?;
    send.write_all(&dir_count.to_le_bytes()).await?;

    // 4. Send the empty directories (file bodies alone would drop them), then
    // stream each file to send, in manifest order.
    for dir in scan.empty_dirs.iter().take(dir_count as usize) {
        let path = dir.as_bytes();
        let path_len = u16::try_from(path.len()).context("directory path too long")?;
        send.write_all(&path_len.to_le_bytes()).await?;
        send.write_all(path).await?;
    }
    for entry in to_send.iter().take(send_count as usize) {
        let source = match kind {
            RootKind::File => scan.canonical.clone(),
            RootKind::Dir => scan.canonical.join(&entry.rel_path),
        };
        send_body(send, &source, entry, throttle).await?;
    }
    super::report(
        narrate,
        "Finished",
        &format!(
            "{} ({})",
            super::count_files(to_send.len()),
            super::human_bytes(total_bytes)
        ),
    );
    Ok(())
}

/// Send one file: `path_len(u16) ‖ path ‖ mode(u32) ‖ mtime(i64) ‖ size(u64) ‖
/// <size bytes> ‖ hash(32)`. Streams the body in bounded chunks and sends
/// exactly the `size` that was hashed (a file that grew is truncated to it; a
/// file that shrank is a hard error).
async fn send_body<W>(send: &mut W, path: &Path, entry: &Entry, throttle: Option<u64>) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let rel = entry.rel_path.as_bytes();
    let rel_len = u16::try_from(rel.len()).context("path too long")?;
    let (mode, mtime) = file_meta(path);
    send.write_all(&rel_len.to_le_bytes()).await?;
    send.write_all(rel).await?;
    send.write_all(&mode.to_le_bytes()).await?;
    send.write_all(&mtime.to_le_bytes()).await?;
    send.write_all(&entry.size.to_le_bytes()).await?;

    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let mut buf = vec![0u8; throttle_chunk(throttle)];
    let mut remaining = entry.size;
    while remaining > 0 {
        let want = min(buf.len(), usize::try_from(remaining).unwrap_or(usize::MAX));
        let read = file.read(&mut buf[..want]).await?;
        if read == 0 {
            bail!("{} shrank during transfer", entry.rel_path);
        }
        send.write_all(&buf[..read]).await?;
        remaining -= read as u64;
        pace(throttle, read).await;
    }
    send.write_all(&entry.hash).await?;
    Ok(())
}

/// The unix permission bits and mtime (seconds since the epoch) for `path`,
/// best-effort — a metadata error falls back to a sane default so a transfer is
/// never blocked by an unreadable timestamp.
fn file_meta(path: &Path) -> (u32, i64) {
    let Ok(meta) = std::fs::metadata(path) else {
        return (0o644, 0);
    };
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::MetadataExt;
        meta.mode()
    };
    #[cfg(not(unix))]
    let mode = 0o644u32;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|dur| i64::try_from(dur.as_secs()).ok())
        .unwrap_or(0);
    (mode, mtime)
}
