mod consume;
mod nfs;
mod produce;
mod scan;
mod ticket;
mod wire;

pub(crate) use consume::attach;
pub(crate) use produce::serve;

/// ALPN for the mount protocol — request/response bi-streams with their own
/// protocol identity, distinct from the one-shot file transfer's `FILE_ALPN`.
pub(crate) const MOUNT_ALPN: &[u8] = b"agent-habilis-swarm/mount/1";

/// Length of the bearer-capability secret carried in a mount ticket.
pub(crate) const SECRET_LEN: usize = 32;

/// Per-request header: the 32-byte bearer secret followed by the 1-byte op.
/// Read off every accepted bi-stream — a bad secret poisons the whole
/// connection; an unknown op drops only that stream.
const REQUEST_HEADER_LEN: usize = SECRET_LEN + 1;

/// Request the manifest: the full dir + file listing with sizes and attrs.
const OP_MANIFEST: u8 = 1;

/// Request a byte range of one file, addressed by its manifest index.
const OP_READ: u8 = 2;

/// Ceiling on the encoded manifest, so a hostile producer can't force an
/// unbounded allocation before the first decode error.
const MAX_MANIFEST_BYTES: u32 = 64 * 1024 * 1024;

/// Ceiling on a single READ. Sized to fit the NFS client's `rsize=131072`
/// with headroom; the producer rejects anything larger without killing the
/// connection.
pub(crate) const MAX_READ_LEN: u32 = 256 * 1024;

// The pre-ticket online wait is identical for every direct off-gossip
// command — reuse `file`'s rather than keeping a fourth copy.
use crate::file::wait_online;

/// Quote one word of a printed, copy-pastable command: plain when every
/// character is clearly shell-safe, single-quoted (embedded `'` escaped
/// POSIX-style) otherwise — an unquoted path with a space would split into
/// two arguments when pasted.
fn shell_word(raw: &str) -> String {
    let plain = !raw.is_empty()
        && raw
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | '~'));
    if plain {
        raw.to_owned()
    } else {
        format!("'{}'", raw.replace('\'', "'\\''"))
    }
}

/// Present the producer's status and the consumer's ready-to-run command on
/// **stdout** — the producer's product (file bytes flow over the network, not
/// stdout), and stderr stays errors-only. Human (default) is cargo-style
/// (`Serving <path>` / `Mount <command>`); `json` is the bare command for
/// machines (no status/colors), unchanged so scripts can capture it.
fn announce(json: bool, serving: &str, command: &str) {
    tracing::info!("serving {serving}");
    if json {
        println!("{command}");
        return;
    }
    crate::util::output::status_out("Serving", serving);
    crate::util::output::status_out("Mount", command);
}

#[cfg(test)]
mod tests {
    use super::consume::RemoteClient;
    use super::ticket::MountTicket;
    use super::{MAX_READ_LEN, produce};
    use crate::lookup::{add_peer_addr, build_participant_endpoint};
    use crate::protocol::swarm::LookupOpts;
    use rand::RngCore;
    use std::path::PathBuf;
    use std::sync::Arc;

    /// A throwaway directory under the OS temp dir (the repo has no `tempfile`
    /// dep); dropped recursively at the end of each test.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("ahsw-mount-test-{}", rand::rng().next_u64()));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// Stand up a loopback producer serving `root` and a client connected to
    /// it. The producer task accepts connections until its endpoint closes.
    async fn producer_and_client(
        root: &std::path::Path,
    ) -> (iroh::Endpoint, RemoteClient, tokio::task::JoinHandle<()>) {
        let (manifest, paths) = super::scan::scan(root).expect("scan");
        let files: Arc<Vec<produce::ServedFile>> = Arc::new(
            paths
                .into_iter()
                .zip(&manifest.files)
                .map(|(abs, entry)| produce::ServedFile {
                    abs,
                    size: entry.size,
                })
                .collect(),
        );
        let manifest_bytes = Arc::new(manifest.encode());
        let (endpoint, ticket, secret) = produce::bind(LookupOpts::loopback())
            .await
            .expect("bind producer");
        let accept_endpoint = endpoint.clone();
        let producer = tokio::spawn(async move {
            while let Some(incoming) = accept_endpoint.accept().await {
                let manifest_bytes = Arc::clone(&manifest_bytes);
                let files = Arc::clone(&files);
                tokio::spawn(async move {
                    let _ =
                        produce::serve_connection(incoming, secret, manifest_bytes, files).await;
                });
            }
        });

        let consumer_endpoint = build_participant_endpoint(&ticket.lookups)
            .await
            .expect("consumer endpoint");
        add_peer_addr(&consumer_endpoint, ticket.addr.clone()).expect("add peer addr");
        let client = RemoteClient::new(consumer_endpoint, ticket);
        (endpoint, client, producer)
    }

    fn fixture_tree() -> TempDir {
        let tmp = TempDir::new();
        std::fs::create_dir_all(tmp.path.join("docs")).unwrap();
        std::fs::write(tmp.path.join("hello.txt"), b"hello world").unwrap();
        std::fs::write(tmp.path.join("docs/guide.md"), b"lazy bytes").unwrap();
        tmp
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn manifest_and_ranged_reads_round_trip() {
        let tree = fixture_tree();
        let (endpoint, client, producer) = producer_and_client(&tree.path).await;

        let manifest = client.fetch_manifest().await.expect("manifest");
        assert_eq!(manifest.files.len(), 2);
        assert_eq!(manifest.dirs.len(), 1);
        let hello = manifest
            .files
            .iter()
            .position(|file| file.rel_path == "hello.txt")
            .expect("hello.txt listed");
        let hello = u32::try_from(hello).expect("index");

        let head = client.read_range(hello, 0, 5).await.expect("read");
        assert_eq!(head, b"hello");
        let tail = client.read_range(hello, 6, 100).await.expect("read tail");
        assert_eq!(tail, b"world");
        let past = client.read_range(hello, 999, 4).await.expect("past eof");
        assert!(past.is_empty(), "past-EOF read is a valid empty read");

        endpoint.close().await;
        producer.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bad_index_and_oversize_len_error_without_killing_the_connection() {
        let tree = fixture_tree();
        let (endpoint, client, producer) = producer_and_client(&tree.path).await;

        assert!(client.read_range(424_242, 0, 4).await.is_err(), "bad index");
        assert!(
            client.read_range(0, 0, MAX_READ_LEN + 1).await.is_err(),
            "len over cap"
        );
        // The connection survived both rejections.
        let data = client.read_range(0, 0, 4).await.expect("still serving");
        assert_eq!(data.len(), 4);

        endpoint.close().await;
        producer.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bad_secret_is_rejected() {
        let tree = fixture_tree();
        let (endpoint, client, producer) = producer_and_client(&tree.path).await;

        // A ticket with a corrupted bearer secret: the producer closes the
        // connection, so the request fails rather than answering.
        let bad_ticket = MountTicket {
            addr: client.producer_addr(),
            secret: [0u8; super::SECRET_LEN],
            lookups: LookupOpts::loopback(),
        };
        let bad_endpoint = build_participant_endpoint(&bad_ticket.lookups)
            .await
            .expect("bad-client endpoint");
        add_peer_addr(&bad_endpoint, bad_ticket.addr.clone()).expect("add peer addr");
        let bad = RemoteClient::new(bad_endpoint, bad_ticket);
        assert!(bad.fetch_manifest().await.is_err(), "bad secret must fail");

        // The honest client still works — the producer keeps accepting.
        assert!(client.fetch_manifest().await.is_ok());

        endpoint.close().await;
        producer.abort();
    }
}
