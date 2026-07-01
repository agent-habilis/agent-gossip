//! `ahsw file` — send a file or directory to a peer over a direct, off-gossip
//! QUIC connection, transferring only what the peer is missing or holds an
//! outdated copy of (a snapshot + delta re-sync). `file send <path>` prints
//! the receiver's `ahsw file get 🐝…` command on stdout, then serves;
//! `file get <ticket>` dials, tells the producer what it already has, and
//! writes the returned files into the destination directory (overwriting, never
//! deleting — a receive, not a mirror). The ticket is a bearer capability (a
//! random secret) carrying the producer's address + the swarm's discovery
//! config, so the consumer needs nothing but the ticket.

use std::time::Duration;

use anyhow::{Context, Result};
use iroh::Endpoint;

use crate::protocol::swarm::{LookupOpts, Swarm};

mod consume;
mod manifest;
mod produce;
mod ticket;
mod walk;
mod wire;

pub(crate) use consume::get;
pub(crate) use produce::send;

/// ALPN for the file protocol — its own protocol identity, distinct from the
/// stdio pipe's `PIPE_ALPN` and the port forwarder's `PORT_ALPN`, so a mismatched
/// dial is rejected at the QUIC handshake instead of desyncing on the wire.
pub(crate) const FILE_ALPN: &[u8] = b"agent-habilis-swarm/file/1";

/// Length of the bearer-capability secret carried in a file ticket.
pub(crate) const SECRET_LEN: usize = 32;

/// The consumer's manifest is length-prefixed on the wire; cap it so a hostile
/// peer can't make us allocate unboundedly before a byte of data has moved.
const MAX_MANIFEST_BYTES: usize = 64 * 1024 * 1024;

/// Whether a served path is a single file or a directory tree — one byte on the
/// wire (0 = file, 1 = dir), telling the consumer whether to create a containing
/// directory named after the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RootKind {
    File,
    Dir,
}

/// Resolve a `--swarm` id to its discovery config (`None` ⇒ a public default),
/// so a transfer traverses the network the way that swarm's members do.
fn swarm_lookups(swarm: Option<&str>) -> Result<LookupOpts> {
    match swarm {
        Some(id) => Ok(id
            .parse::<Swarm>()
            .context("invalid --swarm id")?
            .lookups()
            .clone()),
        None => Ok(LookupOpts::public_preset()),
    }
}

/// Best-effort wait (≤5s) for the endpoint to publish reachable addresses, so a
/// freshly-printed ticket resolves immediately. Never blocks forever.
async fn wait_online(endpoint: &Endpoint) {
    let _ = tokio::time::timeout(Duration::from_secs(5), endpoint.online()).await;
}

/// Present the producer's status and the consumer's ready-to-run command on
/// **stdout** — the producer's product (its stdout carries no data; that flows
/// over the network), and stderr stays errors-only. Human (default) is cargo-style
/// (`Serving <path>` / `Get <command>`); `json` is the bare command for
/// machines (no status/colors), unchanged so scripts can capture it.
fn announce(json: bool, serving: &str, command: &str) {
    tracing::info!("serving {serving}");
    if json {
        println!("{command}");
        return;
    }
    crate::util::output::status_out("Serving", serving);
    crate::util::output::status_out("Get", command);
}

/// Emit a cargo-style lifecycle line (`Connected` / `Sending` / `Finished`) on
/// stdout in human mode; suppressed under `--output json`. Always logged.
fn report(narrate: bool, verb: &str, msg: &str) {
    tracing::info!("{verb}: {msg}");
    if narrate {
        crate::util::output::status_out(verb, msg);
    }
}

/// `1 file` / `N files` — a correctly-pluralized file count for status lines.
fn count_files(count: usize) -> String {
    if count == 1 {
        "1 file".to_owned()
    } else {
        format!("{count} files")
    }
}

/// Format a byte count for humans (`512B`, `1.5KB`, `3.4MB`).
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes}B");
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "human-readable display only, not used for any calculation"
    )]
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1}{}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::produce;
    use crate::lookup::build_participant_endpoint;
    use crate::protocol::swarm::LookupOpts;
    use rand::RngCore;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// A throwaway directory under the OS temp dir (the repo has no `tempfile`
    /// dep); dropped recursively at the end of each test.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("ahsw-file-test-{}", rand::rng().next_u64()));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn write_file(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, contents).expect("write file");
    }

    /// Run one full producer→consumer transfer over two loopback endpoints,
    /// serving `root` into `dest_base`. Returns the consumer's summary string.
    async fn transfer(root: &Path, dest_base: &Path) -> String {
        let (endpoint, ticket, secret) = produce::bind(LookupOpts::loopback())
            .await
            .expect("bind producer");
        let root_owned = root.to_path_buf();
        let producer = tokio::spawn(async move {
            if let Some(incoming) = endpoint.accept().await {
                let _ =
                    produce::serve_connection(incoming, &secret, &root_owned, None, false).await;
            }
            endpoint.close().await;
        });

        let consumer_endpoint = build_participant_endpoint(&ticket.lookups)
            .await
            .expect("consumer endpoint");
        let summary = super::consume::receive(&consumer_endpoint, &ticket, dest_base, None, false)
            .await
            .expect("receive");
        consumer_endpoint.close().await;
        producer.await.expect("producer task");
        summary
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn round_trips_a_nested_tree_byte_for_byte() {
        let src = TempDir::new();
        let root = src.path.join("project");
        write_file(&root.join("readme.md"), b"# hello");
        write_file(&root.join("src/main.rs"), b"fn main() {}");
        write_file(&root.join("src/nested/deep.txt"), &vec![7u8; 100_000]);

        let dst = TempDir::new();
        transfer(&root, &dst.path).await;

        let landed = dst.path.join("project");
        assert_eq!(fs::read(landed.join("readme.md")).unwrap(), b"# hello");
        assert_eq!(fs::read(landed.join("src/main.rs")).unwrap(), b"fn main() {}");
        assert_eq!(
            fs::read(landed.join("src/nested/deep.txt")).unwrap(),
            vec![7u8; 100_000]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_single_file_round_trips() {
        let src = TempDir::new();
        let file = src.path.join("report.pdf");
        write_file(&file, b"PDF-CONTENT");

        let dst = TempDir::new();
        let summary = transfer(&file, &dst.path).await;

        assert_eq!(fs::read(dst.path.join("report.pdf")).unwrap(), b"PDF-CONTENT");
        // Correctly singular — "1 file", never "1 files".
        assert!(summary.contains("1 file,"), "summary: {summary}");
        assert!(!summary.contains("1 files"), "summary: {summary}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn delta_skips_files_the_receiver_already_has() {
        let src = TempDir::new();
        let root = src.path.join("data");
        write_file(&root.join("keep.txt"), b"unchanged");
        write_file(&root.join("changed.txt"), b"new-version");

        // Pre-populate the destination with an identical `keep.txt` and a stale
        // `changed.txt`, so only the two non-matching files should transfer.
        let dst = TempDir::new();
        let landed = dst.path.join("data");
        write_file(&landed.join("keep.txt"), b"unchanged");
        write_file(&landed.join("changed.txt"), b"OLD");

        let summary = transfer(&root, &dst.path).await;

        assert_eq!(fs::read(landed.join("changed.txt")).unwrap(), b"new-version");
        assert_eq!(fs::read(landed.join("keep.txt")).unwrap(), b"unchanged");
        // One file sent (changed.txt), one unchanged (keep.txt).
        assert!(summary.contains("1 file"), "summary: {summary}");
        assert!(summary.contains("1 unchanged"), "summary: {summary}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn zero_diff_sends_nothing() {
        let src = TempDir::new();
        let root = src.path.join("data");
        write_file(&root.join("a.txt"), b"same");
        write_file(&root.join("b.txt"), b"same-too");

        let dst = TempDir::new();
        let landed = dst.path.join("data");
        write_file(&landed.join("a.txt"), b"same");
        write_file(&landed.join("b.txt"), b"same-too");

        let summary = transfer(&root, &dst.path).await;
        assert!(summary.contains("0 file"), "summary: {summary}");
        assert!(summary.contains("2 unchanged"), "summary: {summary}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_empty_directory_round_trips() {
        let src = TempDir::new();
        let root = src.path.join("empty");
        fs::create_dir_all(&root).expect("create empty dir");

        let dst = TempDir::new();
        let summary = transfer(&root, &dst.path).await;

        assert!(dst.path.join("empty").is_dir());
        assert!(summary.contains("0 file"), "summary: {summary}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn empty_subdirectories_are_recreated() {
        let src = TempDir::new();
        let root = src.path.join("tree");
        write_file(&root.join("a.txt"), b"x");
        fs::create_dir_all(root.join("logs")).expect("create empty subdir");
        fs::create_dir_all(root.join("nested/deep/empty")).expect("create nested empty subdir");

        let dst = TempDir::new();
        transfer(&root, &dst.path).await;

        let landed = dst.path.join("tree");
        assert_eq!(fs::read(landed.join("a.txt")).unwrap(), b"x");
        assert!(landed.join("logs").is_dir(), "empty subdir must be recreated");
        assert!(
            landed.join("nested/deep/empty").is_dir(),
            "nested empty subdir must be recreated"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_bad_secret_is_rejected() {
        let src = TempDir::new();
        write_file(&src.path.join("pkg/f.txt"), b"x");

        let (endpoint, mut ticket, secret) = produce::bind(LookupOpts::loopback())
            .await
            .expect("bind producer");
        let root = src.path.join("pkg");
        let producer = tokio::spawn(async move {
            if let Some(incoming) = endpoint.accept().await {
                let _ = produce::serve_connection(incoming, &secret, &root, None, false).await;
            }
            endpoint.close().await;
        });

        // Forge a ticket with the right address but the wrong secret.
        ticket.secret = [0u8; super::SECRET_LEN];
        let dst = TempDir::new();
        let consumer_endpoint = build_participant_endpoint(&ticket.lookups)
            .await
            .expect("consumer endpoint");
        let result =
            super::consume::receive(&consumer_endpoint, &ticket, &dst.path, None, false).await;
        assert!(result.is_err(), "a bad secret must be rejected");
        consumer_endpoint.close().await;
        producer.await.expect("producer task");
    }

    /// A hostile producer that names a body `../escape` must not write outside
    /// the destination — the consumer's `safe_join` guard aborts the transfer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_traversal_path_is_rejected() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Two duplex pipes model the bidirectional stream pair.
        let (mut prod_send, mut cons_recv) = tokio::io::duplex(64 * 1024);
        let (mut cons_send, mut prod_recv) = tokio::io::duplex(64 * 1024);

        let dst = TempDir::new();
        let base = dst.path.clone();
        let consumer = tokio::spawn(async move {
            super::consume::exchange(&mut cons_send, &mut cons_recv, &base, None, false).await
        });

        // Malicious producer: announce a dir "pkg", read the manifest, send a
        // one-file plan, then a body whose path escapes the destination.
        prod_send.write_all(&[1u8]).await.unwrap(); // kind = dir
        prod_send.write_all(&3u16.to_le_bytes()).await.unwrap();
        prod_send.write_all(b"pkg").await.unwrap();
        // Drain the consumer's manifest (u32 len + bytes).
        let mut len_buf = [0u8; 4];
        prod_recv.read_exact(&mut len_buf).await.unwrap();
        let mut manifest = vec![0u8; u32::from_le_bytes(len_buf) as usize];
        prod_recv.read_exact(&mut manifest).await.unwrap();
        // Plan: send_count=1, unchanged=0, total=1, dir_count=0.
        prod_send.write_all(&1u32.to_le_bytes()).await.unwrap();
        prod_send.write_all(&0u32.to_le_bytes()).await.unwrap();
        prod_send.write_all(&1u64.to_le_bytes()).await.unwrap();
        prod_send.write_all(&0u32.to_le_bytes()).await.unwrap();
        // Body header with a traversal path.
        let evil = b"../escape";
        prod_send
            .write_all(&u16::try_from(evil.len()).unwrap().to_le_bytes())
            .await
            .unwrap();
        prod_send.write_all(evil).await.unwrap();
        prod_send.write_all(&0o644u32.to_le_bytes()).await.unwrap();
        prod_send.write_all(&0i64.to_le_bytes()).await.unwrap();
        prod_send.write_all(&1u64.to_le_bytes()).await.unwrap();

        let result = consumer.await.expect("consumer task");
        assert!(result.is_err(), "a traversal path must be rejected");
        // Nothing was written outside the destination.
        assert!(!dst.path.parent().unwrap().join("escape").exists());
    }
}
