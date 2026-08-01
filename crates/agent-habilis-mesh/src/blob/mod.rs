//! The blob channel — direct point-to-point transfer of payloads too large for a
//! gossip frame, off the gossip plane entirely. The producer serves the content,
//! content-addressed by SHA-256, over a dedicated QUIC endpoint and mints a bare
//! [`ticket::BlobTicket`] referencing it. The consumer dials the producer,
//! presents the ticket's bearer secret, and streams the bytes — verified against
//! the advertised hash.
//!
//! Layering: a *transport* parallel to the gossip binding — its own ALPN, its own
//! bearer-secret handshake, its own bare-base58 ticket. The bytes never touch
//! gossip; only the small reference does, and getting that reference to the
//! consumer is the application's job (it is a short string, so a single
//! [`crate::gossip::send_app`] frame carries it).
//!
//! Raw bytes, not text: unlike a frame body, nothing here is UTF-8-constrained or
//! re-encoded, and the transfer streams in bounded chunks so memory stays flat
//! regardless of size.
//!
//! Producing is path-based — [`BlobServer::register`] stream-hashes a file and
//! snapshots it into a spool dir — so an in-memory payload has to be written to a
//! temp file first. A streaming produce path would have to rework hashing and the
//! `MAX_BLOB_BYTES` check, which cannot run before the length is known.

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

/// An opaque content-group id the blob layer keys evictable blobs by.
///
/// Whatever the application groups content by — a task, a job, a conversation —
/// it maps into this; the blob layer never names that type. `evict_content` then
/// drops a whole group at once.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContentId(String);

impl ContentId {
    #[must_use]
    pub fn new(id: &str) -> Self {
        Self(id.to_owned())
    }
}

/// One offload: which file, where to spool it, which content group it belongs to,
/// and the mesh password it inherits (so a scraped ticket cannot be redeemed
/// without it).
#[derive(Debug)]
pub struct OffloadRequest {
    pub path: PathBuf,
    pub spool_dir: PathBuf,
    pub content_id: ContentId,
    pub password: Option<crate::protocol::crypto::Password>,
}

/// Offload a file over the blob channel and return the [`BlobTicket`] that
/// fetches it back. Lazily binds `server` on the first call (into
/// `request.spool_dir`), reusing it thereafter — so a caller holds one
/// `Option<BlobServer>` for its lifetime and passes it here each time.
///
/// The ticket is the whole result: `encode()` it into whatever reference the
/// application uses, ship that string over gossip, and the peer calls
/// [`fetch`]. Content-addressed dedup means offloading identical bytes twice
/// returns the first ticket without re-paying the hash or the Argon2 stretch.
///
/// # Errors
/// Binding the blob server, hashing, or snapshotting the file fails (e.g. the
/// file is unreadable or exceeds `MAX_BLOB_BYTES`).
/// # Panics
/// Panics if an internal invariant is violated.
pub async fn offload(
    server: &mut Option<BlobServer>,
    lookups: &LookupOpts,
    request: OffloadRequest,
) -> Result<BlobTicket> {
    let OffloadRequest {
        path,
        spool_dir,
        content_id,
        password,
    } = request;
    if server.is_none() {
        *server = Some(BlobServer::start(lookups.clone(), spool_dir, password).await?);
    }
    server
        .as_ref()
        .expect("server set above")
        .register(&path, content_id)
        .await
}

/// ALPN for the blob channel — a raw bidirectional QUIC stream with its own
/// protocol identity, distinct from `GOSSIP_ALPN` and the application's own bridge
/// ALPN, so a mismatched dial is rejected at the QUIC handshake.
pub(crate) const BLOB_ALPN: &[u8] = b"habilis-mesh/blob/1";

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
        let path = std::env::temp_dir().join(format!(
            "agent-habilis-mesh-blob-src-{}",
            rand::rng().next_u64()
        ));
        fs::write(&path, bytes).expect("write temp file");
        path
    }

    fn temp_spool() -> PathBuf {
        std::env::temp_dir().join(format!(
            "agent-habilis-mesh-blob-spool-{}",
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

    /// The whole produce → reference → consume path through the **public** surface
    /// only: [`offload`] mints a ticket, `encode`/`decode` round-trips it as the
    /// string an application would ship over gossip, and [`fetch`] streams the
    /// bytes back. Nothing here names an application type, which is the point —
    /// this is the shape any consumer writes, whatever its data model.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn offload_to_fetch_needs_no_application_type() {
        let payload: Vec<u8> = (0..40_000_u32).map(|byte| byte.to_le_bytes()[0]).collect();
        let src = temp_file(&payload);
        let mut server: Option<BlobServer> = None;

        let ticket = super::offload(
            &mut server,
            &LookupOpts::loopback(),
            super::OffloadRequest {
                path: src.clone(),
                spool_dir: temp_spool(),
                content_id: ContentId::new("generic-consumer-group"),
                password: None,
            },
        )
        .await
        .expect("offload");

        // Raw bytes, so no base64/UTF-8 detour: the payload cycles 0..=255,
        // which a frame body would reject outright.
        let wire = ticket.encode();
        let decoded = super::BlobTicket::decode(&wire).expect("ticket round-trips as a string");

        let mut out = Vec::new();
        fetch(&decoded, &mut out, None).await.expect("fetch");
        assert_eq!(out, payload, "bytes must survive byte-for-byte");

        // Content-addressed dedup: the same bytes offload to the same ticket
        // without re-hashing.
        let again = super::offload(
            &mut server,
            &LookupOpts::loopback(),
            super::OffloadRequest {
                path: src.clone(),
                spool_dir: temp_spool(),
                content_id: ContentId::new("generic-consumer-group"),
                password: None,
            },
        )
        .await
        .expect("second offload");
        assert_eq!(again.sha256_hex(), ticket.sha256_hex());

        fs::remove_file(&src).ok();
        if let Some(server) = server {
            server.shutdown().await;
        }
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
