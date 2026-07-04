//! The blob producer: a daemon-hosted, lazily-bound QUIC endpoint that serves
//! this peer's spooled blobs, content-addressed, over its own ALPN. `register`
//! offloads a file (stream-hash off the event loop, snapshot into the spool,
//! mint a ticket); the accept loop serves each fetch by streaming the spool file
//! from disk after a bearer-secret handshake. One endpoint, per-blob capability
//! (the secret is stored with the blob), kept for the daemon's lifetime.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use iroh::Endpoint;
use iroh::endpoint::{Connection, Incoming, RecvStream, SendStream};
use rand::RngCore;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt as _;
use tokio::sync::Mutex;

use crate::a2a::TaskId;
use crate::lookup::build_endpoint;
use crate::protocol::crypto::ct_eq;
use crate::protocol::swarm::LookupOpts;
use crate::util::consts::MAX_BLOB_BYTES;

use super::store::BlobStore;
use super::ticket::BlobTicket;
use super::{BAD_SECRET, DONE, HASH_LEN, SECRET_LEN, UNKNOWN_BLOB, wait_online};

/// Chunk size for hashing and streaming — bounded, so memory is constant
/// regardless of blob size.
const CHUNK: usize = 64 * 1024;

/// The producer side of the blob channel. Owns the serving endpoint and the
/// content-addressed store; both live for the daemon's process lifetime.
pub(crate) struct BlobServer {
    endpoint: Endpoint,
    store: Arc<Mutex<BlobStore>>,
    lookups: LookupOpts,
}

impl BlobServer {
    /// Bind the serving endpoint (its own `BLOB_ALPN` identity, separate from
    /// gossip), create the spool, and spawn the accept loop. Called lazily on the
    /// first offload.
    ///
    /// # Errors
    /// Endpoint bind or spool-directory creation fails.
    pub(crate) async fn start(lookups: LookupOpts, spool_dir: PathBuf) -> Result<Self> {
        let endpoint =
            build_endpoint(&lookups, None, None, vec![super::BLOB_ALPN.to_vec()]).await?;
        if !lookups.is_loopback() {
            wait_online(&endpoint).await;
        }
        let store = Arc::new(Mutex::new(BlobStore::new(spool_dir)?));
        spawn_accept_loop(endpoint.clone(), Arc::clone(&store));
        Ok(Self {
            endpoint,
            store,
            lookups,
        })
    }

    /// Offload `path` under `task_id`: stream-hash it off the event loop, snapshot
    /// it into the spool, mint a per-blob bearer secret, and return the fetch
    /// ticket to embed in a `Part.url`.
    ///
    /// # Errors
    /// The file is unreadable, exceeds `MAX_BLOB_BYTES`, or snapshotting fails.
    pub(crate) async fn register(&self, path: &Path, task_id: TaskId) -> Result<BlobTicket> {
        let (sha256, size) = hash_file(path.to_path_buf()).await?;
        if size > MAX_BLOB_BYTES {
            bail!("file too large to offload ({size} bytes > {MAX_BLOB_BYTES})");
        }
        let mut proposed = [0u8; SECRET_LEN];
        rand::rng().fill_bytes(&mut proposed);
        let secret = self
            .store
            .lock()
            .await
            .snapshot(path, sha256, size, proposed, task_id)?;
        Ok(BlobTicket {
            addr: self.endpoint.addr(),
            secret,
            sha256,
            size,
            lookups: self.lookups.clone(),
            password: false,
        })
    }

    /// Drop every blob owned by `task_id` (the task idle-timeout sweep).
    pub(crate) async fn evict_task(&self, task_id: &TaskId) {
        self.store.lock().await.evict_task(task_id);
    }

    /// Close the endpoint on `leave`/shutdown; dropping `self` drops the store,
    /// which removes the spool directory.
    pub(crate) async fn shutdown(self) {
        self.endpoint.close().await;
    }
}

/// Spawn the accept loop: each inbound connection is one fetch, served on its own
/// task so a slow transfer never blocks the next.
fn spawn_accept_loop(endpoint: Endpoint, store: Arc<Mutex<BlobStore>>) {
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let store = Arc::clone(&store);
            tokio::spawn(async move {
                if let Err(error) = serve_connection(incoming, &store).await {
                    tracing::debug!(%error, "blob serve connection ended");
                }
            });
        }
    });
}

/// Serve one fetch connection: accept its single bi-stream and answer it.
async fn serve_connection(incoming: Incoming, store: &Mutex<BlobStore>) -> Result<()> {
    let conn = incoming.await?;
    let (send, recv) = conn.accept_bi().await?;
    serve_stream(&conn, send, recv, store).await
}

/// The fetch protocol, producer side. Reads the fixed 64-byte request
/// (`sha256 ‖ secret`), authenticates against the addressed blob's secret, then
/// streams `size ‖ <bytes>` from the spool file. A bad secret or unknown hash
/// closes the connection with a coded reason.
async fn serve_stream(
    conn: &Connection,
    mut send: SendStream,
    mut recv: RecvStream,
    store: &Mutex<BlobStore>,
) -> Result<()> {
    let mut request = [0u8; HASH_LEN + SECRET_LEN];
    if recv.read_exact(&mut request).await.is_err() {
        return Ok(());
    }
    let mut hash = [0u8; HASH_LEN];
    hash.copy_from_slice(&request[..HASH_LEN]);
    let mut token = [0u8; SECRET_LEN];
    token.copy_from_slice(&request[HASH_LEN..]);

    // Resolve under the lock, then release it before streaming from disk.
    let served = {
        let guard = store.lock().await;
        match guard.get(&hash) {
            None => None,
            Some(entry) if ct_eq(&token, &entry.secret) => Some((entry.path.clone(), entry.size)),
            Some(_) => {
                conn.close(BAD_SECRET.into(), b"bad secret");
                return Ok(());
            }
        }
    };
    let Some((path, size)) = served else {
        conn.close(UNKNOWN_BLOB.into(), b"unknown blob");
        return Ok(());
    };

    send.write_all(&size.to_le_bytes()).await?;
    stream_file(&path, &mut send).await?;
    let _ = send.finish();
    // Let the consumer read the tail before CONNECTION_CLOSE races it on a fast link.
    let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
    conn.close(DONE.into(), b"done");
    Ok(())
}

/// Stream `path` to `send` in bounded chunks — constant memory.
async fn stream_file(path: &Path, send: &mut SendStream) -> Result<()> {
    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("opening spooled blob {} failed", path.display()))?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let read = file
            .read(&mut buf)
            .await
            .context("reading spooled blob failed")?;
        if read == 0 {
            break;
        }
        send.write_all(&buf[..read]).await?;
    }
    Ok(())
}

/// Read + SHA-256-hash a file off the async runtime (`spawn_blocking`), never
/// loading it whole. Returns the digest and byte length.
async fn hash_file(path: PathBuf) -> Result<([u8; HASH_LEN], u64)> {
    tokio::task::spawn_blocking(move || {
        let mut file = std::fs::File::open(&path)
            .with_context(|| format!("opening {} failed", path.display()))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; CHUNK];
        let mut size = 0u64;
        loop {
            let read = file
                .read(&mut buf)
                .context("reading file for hashing failed")?;
            if read == 0 {
                break;
            }
            hasher.update(&buf[..read]);
            size += read as u64;
        }
        let digest = hasher.finalize();
        let mut sha256 = [0u8; HASH_LEN];
        sha256.copy_from_slice(&digest);
        Ok((sha256, size))
    })
    .await
    .context("blob hash task panicked")?
}
