//! The blob channel — direct point-to-point transfer of large task
//! artifacts/attachments, off the gossip plane. A file too big to inline in a
//! gossip frame is offloaded here: the producer's daemon serves the content,
//! content-addressed by SHA-256, over a dedicated QUIC endpoint, and hands the
//! consumer a `💬` [`ticket::BlobTicket`] reference (placed in an A2A
//! `Part.url`). The consumer dials the producer, presents the ticket's bearer
//! secret, and streams the bytes — verified against the advertised hash.
//!
//! Layering: this is a *transport* under the A2A layer, parallel to the gossip
//! binding, mirroring the a2a bridge (its expose/connect endpoints) — its own
//! ALPN, its own bearer-secret handshake, its own emoji-namespaced ticket. The
//! bytes never touch gossip; only the small reference does.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use iroh::Endpoint;

use crate::protocol::mesh::LookupOpts;

mod consume;
mod produce;
mod store;
mod ticket;

pub use consume::fetch;
pub use produce::BlobServer;
pub use ticket::BlobTicket;

/// An opaque content-group id the blob layer keys evictable blobs by. The a2a
/// layer maps its `TaskId` into this; the blob layer never names the a2a type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContentId(String);

impl ContentId {
    #[must_use]
    pub fn new(id: &str) -> Self {
        Self(id.to_owned())
    }
}

/// A blob offload's reference fields, for the a2a layer to wrap into an A2A `Part`.
#[derive(Debug)]
pub struct BlobPart {
    pub url: String,
    pub filename: Option<String>,
    pub media_type: Option<String>,
}

/// A file to offload onto an A2A part: the path plus the optional
/// `filename`/`mediaType` to advertise (the `--file` / `--file-name` /
/// `--file-mime` surface).
#[derive(Debug)]
pub struct FileRef {
    pub path: PathBuf,
    pub name: Option<String>,
    pub mime: Option<String>,
}

/// Offload `file` over the blob channel and return an A2A `Part` that references
/// it by `url` (a `💬` ticket) — for an output `Artifact.parts` or an input
/// `Message.parts`. Lazily binds the daemon's blob server on the first offload
/// (into `spool_dir`), reusing it thereafter.
///
/// # Errors
/// Binding the blob server, hashing, or snapshotting the file fails (e.g. the
/// file is unreadable or exceeds `MAX_BLOB_BYTES`).
/// # Panics
/// Panics if an internal invariant is violated.
pub async fn url_part(
    file: FileRef,
    server: &mut Option<BlobServer>,
    lookups: &LookupOpts,
    spool_dir: PathBuf,
    content_id: ContentId,
    password: Option<crate::protocol::crypto::Password>,
) -> Result<BlobPart> {
    if server.is_none() {
        *server = Some(BlobServer::start(lookups.clone(), spool_dir, password).await?);
    }
    let ticket = server
        .as_ref()
        .expect("server set above")
        .register(&file.path, content_id)
        .await?;
    Ok(BlobPart {
        url: ticket.encode(),
        filename: file.name,
        media_type: file.mime,
    })
}

/// ALPN for the blob channel — a raw bidirectional QUIC stream with its own
/// protocol identity, distinct from `GOSSIP_ALPN` and the a2a bridge's
/// `A2A_ALPN`, so a mismatched dial is rejected at the QUIC handshake.
pub(crate) const BLOB_ALPN: &[u8] = b"agent-mesh/blob/1";

/// Length of the bearer-capability secret carried in a blob ticket, and of the
/// auth token opening the fetch stream (the raw secret, or its Argon2id stretch
/// when passworded — same size either way).
pub const SECRET_LEN: usize = 32;

/// Length of the SHA-256 content hash that addresses a blob.
pub const HASH_LEN: usize = 32;

/// Fetch stream close code: the presented bearer secret matched no blob's secret.
pub(crate) const BAD_SECRET: u32 = 1;

/// Fetch stream close code: the requested content hash is not in the store.
pub(crate) const UNKNOWN_BLOB: u32 = 2;

/// Fetch stream close code: an orderly done from the producer.
pub(crate) const DONE: u32 = 0;

/// Best-effort wait (≤5s) for the endpoint to publish reachable addresses, so a
/// freshly-minted ticket resolves immediately. Never blocks forever.
async fn wait_online(endpoint: &Endpoint) {
    let _ = tokio::time::timeout(Duration::from_secs(5), endpoint.online()).await;
}

#[cfg(test)]
mod tests {
    use super::{BlobServer, ContentId, fetch};
    use crate::protocol::mesh::LookupOpts;
    use rand::RngCore;
    use std::fs;
    use std::path::PathBuf;

    /// A throwaway file under the OS temp dir holding `bytes`; the caller drops it.
    fn temp_file(bytes: &[u8]) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("agent-mesh-blob-src-{}", rand::rng().next_u64()));
        fs::write(&path, bytes).expect("write temp file");
        path
    }

    fn temp_spool() -> PathBuf {
        std::env::temp_dir().join(format!(
            "agent-mesh-blob-spool-{}",
            rand::rng().next_u64()
        ))
    }

    /// Start a loopback producer serving `payload`, fetch it back over a second
    /// loopback endpoint, and return the fetched bytes.
    async fn round_trip(payload: &[u8]) -> Vec<u8> {
        let server = BlobServer::start(LookupOpts::loopback(), temp_spool(), None)
            .await
            .expect("start producer");
        let src = temp_file(payload);
        let ticket = server
            .register(&src, ContentId::new("blob-test-content"))
            .await
            .expect("register");
        let mut out = Vec::new();
        fetch(&ticket, &mut out, None).await.expect("fetch");
        fs::remove_file(&src).ok();
        server.shutdown().await;
        out
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn round_trips_a_blob_byte_for_byte() {
        let payload = vec![7u8; 200_000];
        assert_eq!(round_trip(&payload).await, payload);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn round_trips_an_empty_blob() {
        assert_eq!(round_trip(b"").await, b"");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn passworded_blob_requires_the_password() {
        use crate::protocol::crypto::Password;
        let password = Password::new("hunter2".to_owned());
        let server =
            BlobServer::start(LookupOpts::loopback(), temp_spool(), Some(password.clone()))
                .await
                .unwrap();
        let src = temp_file(b"secret bytes");
        let ticket = server
            .register(&src, ContentId::new("blob-test-content"))
            .await
            .unwrap();
        assert!(ticket.password, "the ticket must be password-protected");

        // Missing password and a wrong password are both refused.
        let mut out = Vec::new();
        assert!(
            fetch(&ticket, &mut out, None).await.is_err(),
            "a passworded ticket must not redeem without the password"
        );
        out.clear();
        assert!(
            fetch(&ticket, &mut out, Some(Password::new("wrong".to_owned())))
                .await
                .is_err(),
            "a wrong password must be refused"
        );
        // The mesh password fetches byte-for-byte.
        out.clear();
        fetch(&ticket, &mut out, Some(password))
            .await
            .expect("fetch");
        assert_eq!(out, b"secret bytes");

        fs::remove_file(&src).ok();
        server.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_bad_secret_is_refused() {
        let server = BlobServer::start(LookupOpts::loopback(), temp_spool(), None)
            .await
            .unwrap();
        let src = temp_file(b"secret payload");
        let mut ticket = server
            .register(&src, ContentId::new("blob-test-content"))
            .await
            .unwrap();
        ticket.secret = [0u8; super::SECRET_LEN]; // forge a wrong bearer secret
        let mut out = Vec::new();
        assert!(
            fetch(&ticket, &mut out, None).await.is_err(),
            "a wrong secret must be refused"
        );
        fs::remove_file(&src).ok();
        server.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unknown_hash_is_refused() {
        let server = BlobServer::start(LookupOpts::loopback(), temp_spool(), None)
            .await
            .unwrap();
        let src = temp_file(b"real content");
        let mut ticket = server
            .register(&src, ContentId::new("blob-test-content"))
            .await
            .unwrap();
        ticket.sha256 = [0xabu8; super::HASH_LEN]; // a hash the store doesn't hold
        let mut out = Vec::new();
        assert!(
            fetch(&ticket, &mut out, None).await.is_err(),
            "an unknown hash must be refused"
        );
        fs::remove_file(&src).ok();
        server.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_size_disagreement_is_rejected() {
        let server = BlobServer::start(LookupOpts::loopback(), temp_spool(), None)
            .await
            .unwrap();
        let src = temp_file(b"exactly this many bytes");
        let mut ticket = server
            .register(&src, ContentId::new("blob-test-content"))
            .await
            .unwrap();
        ticket.size += 1; // producer will offer the true size, which won't match
        let mut out = Vec::new();
        assert!(
            fetch(&ticket, &mut out, None).await.is_err(),
            "a size disagreement must be rejected"
        );
        fs::remove_file(&src).ok();
        server.shutdown().await;
    }
}
