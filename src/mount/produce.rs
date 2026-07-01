use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use iroh::Endpoint;
use iroh::endpoint::{Connection, Incoming, RecvStream, SendStream};
use rand::RngCore;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::file::human_bytes;
use crate::lookup::build_endpoint;
use crate::protocol::swarm::LookupOpts;

use super::ticket::MountTicket;
use super::wire::ReadStatus;
use super::{
    MAX_READ_LEN, MOUNT_ALPN, OP_MANIFEST, OP_READ, REQUEST_HEADER_LEN, SECRET_LEN, wait_online,
};

/// One servable file: the absolute path READs open, and the size the scan
/// recorded (offsets are clamped against it — snapshot semantics).
pub(super) struct ServedFile {
    pub abs: PathBuf,
    pub size: u64,
}

/// Producer: share `dir` read-only. Scans **once** at startup — a consistent
/// snapshot with stable READ indices for every consumer and reconnect — then
/// prints the consumer's `ahsw mount` command on stdout and serves manifest
/// and ranged-read requests until interrupted.
///
/// # Errors
/// `dir` is not a readable directory, or the endpoint fails to bind.
pub(crate) async fn serve(swarm: Option<&str>, dir: &Path, json: bool) -> Result<()> {
    let root = dir
        .canonicalize()
        .with_context(|| format!("resolving {}", dir.display()))?;
    if !root.is_dir() {
        bail!("mount serves a directory; use `ahsw file send` for a single file");
    }
    let (manifest, paths) = super::scan::scan(&root)?;
    let file_count = manifest.files.len();
    let total_bytes: u64 = manifest.files.iter().map(|file| file.size).sum();
    let files: Arc<Vec<ServedFile>> = Arc::new(
        paths
            .into_iter()
            .zip(&manifest.files)
            .map(|(abs, entry)| ServedFile {
                abs,
                size: entry.size,
            })
            .collect(),
    );
    let encoded = manifest.encode();
    // Enforce the consumer-side cap here too: past it, every redeem would
    // abort with "manifest too large" — fail at serve time with a reason
    // instead of minting a ticket nobody can use.
    if encoded.len() > usize::try_from(super::MAX_MANIFEST_BYTES).expect("u32 fits usize") {
        bail!(
            "tree too large to serve: the manifest is {} for {file_count} files (cap {})",
            human_bytes(u64::try_from(encoded.len()).expect("usize fits u64")),
            human_bytes(u64::from(super::MAX_MANIFEST_BYTES))
        );
    }
    let manifest_bytes = Arc::new(encoded);

    let lookups = super::swarm_lookups(swarm)?;
    let (endpoint, ticket, secret) = bind(lookups).await?;
    // Shell-quoted: the hint is printed for copy-paste (and captured verbatim
    // by scripts in json mode), so a dir name with a space must stay one word.
    let mount_hint = super::shell_word(
        &root
            .file_name()
            .and_then(|name| name.to_str())
            .map_or_else(|| "./mnt".to_owned(), |name| format!("./{name}")),
    );
    super::announce(
        json,
        &format!(
            "{} ({file_count} files, {}, read-only)",
            root.display(),
            human_bytes(total_bytes)
        ),
        &format!("ahsw mount {} {mount_hint}", ticket.encode()),
    );

    while let Some(incoming) = endpoint.accept().await {
        let manifest_bytes = Arc::clone(&manifest_bytes);
        let files = Arc::clone(&files);
        tokio::spawn(async move {
            if let Err(error) = serve_connection(incoming, secret, manifest_bytes, files).await {
                tracing::debug!(%error, "mount connection ended");
            }
        });
    }
    // The accept loop ended (endpoint closed) — shut down gracefully.
    endpoint.close().await;
    Ok(())
}

/// Bind the producer endpoint and mint its ticket + secret — no I/O, no print.
pub(super) async fn bind(lookups: LookupOpts) -> Result<(Endpoint, MountTicket, [u8; SECRET_LEN])> {
    let endpoint = build_endpoint(&lookups, None, None, vec![MOUNT_ALPN.to_vec()]).await?;
    // Loopback needs no online wait (the bound addr is immediately usable).
    if !lookups.is_loopback() {
        wait_online(&endpoint).await;
    }
    let mut secret = [0u8; SECRET_LEN];
    rand::rng().fill_bytes(&mut secret);
    let ticket = MountTicket {
        addr: endpoint.addr(),
        secret,
        lookups,
    };
    Ok((endpoint, ticket, secret))
}

/// Accept one inbound connection and serve each of its bi-streams as an
/// independent request. Long-lived: a mounted filesystem issues reads for as
/// long as it stays mounted. Ends when the peer closes the connection (or a
/// bad secret forces it closed from `serve_stream`).
pub(super) async fn serve_connection(
    incoming: Incoming,
    secret: [u8; SECRET_LEN],
    manifest_bytes: Arc<Vec<u8>>,
    files: Arc<Vec<ServedFile>>,
) -> Result<()> {
    let conn = incoming.await.context("incoming connection failed")?;
    // `accept_bi` errors once the connection is gone (peer closed, or a bad
    // secret closed it from within a stream task) — that ends the loop.
    while let Ok((send, recv)) = conn.accept_bi().await {
        let conn = conn.clone();
        let manifest_bytes = Arc::clone(&manifest_bytes);
        let files = Arc::clone(&files);
        tokio::spawn(async move {
            if let Err(error) =
                serve_stream(&conn, send, recv, &secret, &manifest_bytes, &files).await
            {
                tracing::debug!(%error, "mount stream ended");
            }
        });
    }
    Ok(())
}

/// Authenticate one bi-stream by its 33-byte header and answer the request.
/// A bad secret closes the whole connection (the bearer is poisoned); an
/// unknown op or a malformed request drops only this stream.
async fn serve_stream(
    conn: &Connection,
    mut send: SendStream,
    mut recv: RecvStream,
    secret: &[u8; SECRET_LEN],
    manifest_bytes: &[u8],
    files: &[ServedFile],
) -> Result<()> {
    let mut header = [0u8; REQUEST_HEADER_LEN];
    if recv.read_exact(&mut header).await.is_err() {
        // The stream died before delivering a full header — nothing to serve.
        return Ok(());
    }
    if &header[..SECRET_LEN] != secret {
        conn.close(1u32.into(), b"bad secret");
        return Ok(());
    }
    match header[SECRET_LEN] {
        OP_MANIFEST => {
            send.write_all(&[ReadStatus::Ok.to_byte()]).await?;
            let len = u32::try_from(manifest_bytes.len()).context("manifest too large")?;
            send.write_all(&len.to_le_bytes()).await?;
            send.write_all(manifest_bytes).await?;
        }
        OP_READ => {
            let mut request = [0u8; 16];
            if recv.read_exact(&mut request).await.is_err() {
                return Ok(());
            }
            let index = u32::from_le_bytes(request[..4].try_into().expect("4 bytes"));
            let offset = u64::from_le_bytes(request[4..12].try_into().expect("8 bytes"));
            let len = u32::from_le_bytes(request[12..].try_into().expect("4 bytes"));
            let (status, data) = answer_read(files, index, offset, len).await;
            send.write_all(&[status.to_byte()]).await?;
            let data_len = u32::try_from(data.len()).expect("bounded by MAX_READ_LEN");
            send.write_all(&data_len.to_le_bytes()).await?;
            send.write_all(&data).await?;
        }
        other => {
            // Unknown op: drop just this stream, keep the connection.
            tracing::debug!(op = other, "rejecting unknown mount op");
            return Ok(());
        }
    }
    // `finish` only marks the stream done; wait (briefly) for the consumer's
    // ACK so a fast/loopback connection doesn't race the stream teardown ahead
    // of the last bytes.
    let _ = send.finish();
    let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
    Ok(())
}

/// Serve one ranged read. Opens the file per request — simple, correct, and
/// no fd table held hostage by however many files a consumer touches; the OS
/// dentry/page cache makes the reopen cheap.
async fn answer_read(
    files: &[ServedFile],
    index: u32,
    offset: u64,
    len: u32,
) -> (ReadStatus, Vec<u8>) {
    if len > MAX_READ_LEN {
        return (ReadStatus::LenOverCap, Vec::new());
    }
    let Some(file) = usize::try_from(index)
        .ok()
        .and_then(|index| files.get(index))
    else {
        return (ReadStatus::BadIndex, Vec::new());
    };
    if offset >= file.size {
        // Past the snapshot's EOF is a valid empty read, not an error.
        return (ReadStatus::Ok, Vec::new());
    }
    let want = usize::try_from(u64::from(len).min(file.size - offset)).expect("bounded by len");
    match read_range(&file.abs, offset, want).await {
        Ok(data) => (ReadStatus::Ok, data),
        Err(error) => {
            tracing::warn!(%error, path = %file.abs.display(), "read failed");
            (ReadStatus::Io, Vec::new())
        }
    }
}

async fn read_range(path: &Path, offset: u64, want: usize) -> Result<Vec<u8>> {
    let mut file = tokio::fs::File::open(path).await?;
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut data = vec![0u8; want];
    let mut filled = 0;
    // A plain read loop instead of `read_exact`: a file that shrank since the
    // scan yields a short (not failed) read — snapshot semantics.
    while filled < want {
        let read = file.read(&mut data[filled..]).await?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    data.truncate(filled);
    Ok(data)
}
