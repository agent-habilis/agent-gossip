//! The file receiver: redeem a ticket, tell the sender what it already has,
//! and write the files it sends back into the destination directory.

use std::cmp::min;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use iroh::Endpoint;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::lookup::{add_peer_addr, build_participant_endpoint};
use crate::util::progress::{Progress, pace, throttle_chunk};
use crate::protocol::crypto::{Password, TicketAuth};

use super::manifest::HASH_LEN;
use super::ticket::FileTicket;
use super::walk::{manifest_of_dir, manifest_of_file, safe_component, safe_join};
use super::wire::{read_i64, read_str, read_u32, read_u64};
use super::{FILE_ALPN, RootKind, human_bytes};

/// How long to keep retrying the dial while the producer's address propagates
/// (mDNS is instant on a LAN; the DHT fallback can take tens of seconds).
const DISCOVERY_DEADLINE: Duration = Duration::from_secs(90);
const RETRY_DELAY: Duration = Duration::from_secs(3);

/// Redeem `ticket` and receive the tree into `out` (or the current directory),
/// overwriting changed/new files and skipping unchanged ones.
///
/// # Errors
/// A malformed ticket, a password mismatch with the ticket (missing on a
/// passworded ticket, or given for a passwordless one), an unreachable
/// producer, a corrupt transfer (a body's hash didn't match), or a
/// filesystem error.
pub(crate) async fn get(
    ticket: &str,
    out: Option<&Path>,
    throttle: Option<u64>,
    json: bool,
    password: Option<Password>,
) -> Result<()> {
    let ticket = FileTicket::decode(ticket)?;
    let auth = ticket_auth(&ticket, password.as_ref())?;
    let endpoint = build_participant_endpoint(&ticket.lookups).await?;
    let base = match out {
        Some(dir) => dir.to_path_buf(),
        None => std::env::current_dir().context("resolving the current directory failed")?,
    };
    let result = receive(&endpoint, &ticket, &auth, &base, throttle, !json).await;
    match result {
        Ok(summary) => {
            if !json {
                crate::util::output::status_out("Received", &summary);
            }
            // The transfer is complete and the connection is closed; exit now
            // rather than awaiting `endpoint.close()` (which tears down
            // relay/DHT/mDNS and lingers for seconds).
            std::process::exit(0);
        }
        Err(error) => {
            endpoint.close().await;
            Err(error)
        }
    }
}

/// The token this consumer must present for `ticket`: the raw secret, or
/// the Argon2id password stretch when the ticket is passworded. Rejects a
/// missing password and a password offered to a passwordless ticket.
pub(super) fn ticket_auth(ticket: &FileTicket, password: Option<&Password>) -> Result<TicketAuth> {
    match (password, ticket.password) {
        (None, true) => bail!("this ticket is password-protected — pass --password"),
        (Some(_), false) => bail!("this ticket has no password — drop --password"),
        _ => Ok(TicketAuth::derive(&ticket.secret, password)),
    }
}

/// Dial the producer (retrying while its address propagates), open the bi-stream,
/// and present the auth token (the raw bearer secret, or its password stretch).
async fn dial_and_handshake(
    endpoint: &Endpoint,
    ticket: &FileTicket,
    auth: &TicketAuth,
) -> Result<(Connection, SendStream, RecvStream)> {
    add_peer_addr(endpoint, ticket.addr.clone())?;
    let start = Instant::now();
    let conn = loop {
        match endpoint.connect(ticket.addr.clone(), FILE_ALPN).await {
            Ok(conn) => break conn,
            Err(error) if start.elapsed() < DISCOVERY_DEADLINE => {
                tracing::warn!(%error, "connect failed; retrying");
                tokio::time::sleep(RETRY_DELAY).await;
            }
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "could not reach the file producer: {error}"
                ));
            }
        }
    };
    let (mut send, recv) = conn.open_bi().await.context("opening the stream failed")?;
    send.write_all(&auth.token)
        .await
        .context("sending the ticket auth token failed")?;
    Ok((conn, send, recv))
}

/// The consumer half of the protocol: read the tree's kind + name, send our
/// manifest, then receive the diff. Returns a one-line human summary. Generic
/// over the streams so it is loopback-testable.
pub(super) async fn receive(
    endpoint: &Endpoint,
    ticket: &FileTicket,
    auth: &TicketAuth,
    base: &Path,
    throttle: Option<u64>,
    narrate: bool,
) -> Result<String> {
    let (conn, mut send, mut recv) = dial_and_handshake(endpoint, ticket, auth).await?;
    let summary = match exchange(&mut send, &mut recv, base, throttle, narrate).await {
        Ok(summary) => summary,
        Err(error) => {
            // The producer rejects a bad token by closing the connection,
            // which the first framed read observes — surface the coded
            // reason over the generic read failure.
            return Err(match conn.close_reason() {
                Some(iroh::endpoint::ConnectionError::ApplicationClosed(close))
                    if u64::from(close.error_code) == 3 =>
                {
                    anyhow::anyhow!("the producer rejected the password (wrong password)")
                }
                Some(iroh::endpoint::ConnectionError::ApplicationClosed(close))
                    if u64::from(close.error_code) == 1 =>
                {
                    anyhow::anyhow!(
                        "the producer rejected the ticket secret (corrupt or stale ticket)"
                    )
                }
                _ => error,
            });
        }
    };
    let _ = send.finish();
    let _ = send.stopped().await;
    conn.close(0u32.into(), b"done");
    Ok(summary)
}

/// The framed exchange over an established, authenticated stream pair.
pub(super) async fn exchange<W, R>(
    send: &mut W,
    recv: &mut R,
    base: &Path,
    throttle: Option<u64>,
    narrate: bool,
) -> Result<String>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    // 1. What are we receiving — a single file or a directory tree?
    let mut kind_buf = [0u8; 1];
    recv.read_exact(&mut kind_buf).await?;
    let kind = match kind_buf[0] {
        0 => RootKind::File,
        1 => RootKind::Dir,
        other => bail!("unknown root kind byte from peer: {other}"),
    };
    let name = read_str(recv).await?;
    // The container name is attacker-controlled: it must be one safe component.
    safe_component(&name)?;

    // Decide where bodies land and build our manifest of what we already have.
    let (write_base, ours) = match kind {
        RootKind::Dir => {
            let root = base.join(&name);
            std::fs::create_dir_all(&root)
                .with_context(|| format!("creating {}", root.display()))?;
            let manifest = manifest_of_dir(&root)?;
            (root, manifest)
        }
        RootKind::File => (base.to_path_buf(), manifest_of_file(base, &name)?),
    };

    // 2. Send our manifest so the producer only sends what we're missing.
    let encoded = ours.encode();
    send.write_all(
        &u32::try_from(encoded.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    )
    .await?;
    send.write_all(&encoded).await?;

    // 3. Read the plan.
    let send_count = read_u32(recv).await?;
    let unchanged = read_u32(recv).await?;
    let total = read_u64(recv).await?;
    let dir_count = read_u32(recv).await?;

    // 4. Recreate empty directories (file bodies alone would drop them), then
    // receive each body.
    for _ in 0..dir_count {
        let rel = read_str(recv).await?;
        let dir = safe_join(&write_base, &rel)?;
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut progress = Progress::new((total > 0).then_some(total));
    let mut received = 0u64;
    for _ in 0..send_count {
        recv_body(recv, &write_base, throttle, &mut progress, &mut received).await?;
    }
    progress.finish();
    if narrate {
        tracing::info!(send_count, unchanged, total, "file transfer complete");
    }
    Ok(format!(
        "{}, {unchanged} unchanged ({})",
        super::count_files(usize::try_from(send_count).unwrap_or(usize::MAX)),
        human_bytes(total)
    ))
}

/// Receive one body into a temp file, verify its hash, then atomically rename it
/// over the destination. Writing to a sibling temp and swapping means an
/// interrupted or corrupt transfer (a hash mismatch, a dropped connection, a full
/// disk) leaves any pre-existing good copy untouched, and never a half-written
/// file at the real path.
async fn recv_body<R>(
    recv: &mut R,
    write_base: &Path,
    throttle: Option<u64>,
    progress: &mut Progress,
    received: &mut u64,
) -> Result<()>
where
    R: AsyncRead + Unpin,
{
    let rel = read_str(recv).await?;
    // Attacker-controlled path: reject anything that could escape `write_base`.
    let full: PathBuf = safe_join(write_base, &rel)?;
    let mode = read_u32(recv).await?;
    let _mtime = read_i64(recv).await?; // carried for forward-compat; not applied yet
    let size = read_u64(recv).await?;

    let parent = full.parent().context("received path has no parent")?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let tmp = temp_path(&full);

    // Stream the body to the temp file; on ANY failure remove the temp so a
    // failed transfer never leaves a stray partial behind.
    let got = match stream_to_temp(recv, &tmp, size, throttle, progress, received).await {
        Ok(got) => got,
        Err(error) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(error);
        }
    };
    let mut expected = [0u8; HASH_LEN];
    if let Err(error) = recv.read_exact(&mut expected).await {
        let _ = std::fs::remove_file(&tmp);
        return Err(
            anyhow::Error::new(error).context(format!("reading the hash trailer for {rel}"))
        );
    }
    if got != expected {
        let _ = std::fs::remove_file(&tmp);
        bail!("hash mismatch for {rel} — transfer corrupted");
    }
    apply_mode(&tmp, mode);
    std::fs::rename(&tmp, &full).with_context(|| format!("renaming into {}", full.display()))?;
    Ok(())
}

/// Stream `size` bytes from `recv` into a fresh file at `tmp`, hashing as it goes,
/// and return the sha256 of what was written.
async fn stream_to_temp<R>(
    recv: &mut R,
    tmp: &Path,
    size: u64,
    throttle: Option<u64>,
    progress: &mut Progress,
    received: &mut u64,
) -> Result<[u8; HASH_LEN]>
where
    R: AsyncRead + Unpin,
{
    let mut file = tokio::fs::File::create(tmp)
        .await
        .with_context(|| format!("creating {}", tmp.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; throttle_chunk(throttle)];
    let mut remaining = size;
    while remaining > 0 {
        let want = min(buf.len(), usize::try_from(remaining).unwrap_or(usize::MAX));
        let read = recv.read(&mut buf[..want]).await?;
        if read == 0 {
            bail!("connection closed mid-file");
        }
        file.write_all(&buf[..read]).await?;
        hasher.update(&buf[..read]);
        remaining -= read as u64;
        *received += read as u64;
        progress.update(*received);
        pace(throttle, read).await;
    }
    file.flush().await?;
    Ok(hasher.finalize().into())
}

/// A sibling temp path for `full`, in the same directory so the later rename is
/// an atomic same-filesystem swap. The pid keeps concurrent `file get`
/// processes writing the same destination from colliding.
fn temp_path(full: &Path) -> PathBuf {
    let name = full
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    full.with_file_name(format!(".{name}.{}.ahsw-tmp", std::process::id()))
}

/// Apply the sender's unix permission bits, best-effort — a failure never fails
/// the transfer (the file's content is already written and verified).
#[cfg(unix)]
fn apply_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o777));
}

#[cfg(not(unix))]
fn apply_mode(_path: &Path, _mode: u32) {}
