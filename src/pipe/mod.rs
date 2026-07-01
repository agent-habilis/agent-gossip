//! `ahsw pipe` — a P2P byte stream over a dedicated direct
//! QUIC connection, off the gossip log. The producer ([`listen`]) reads stdin
//! and prints the consumer's `ahsw pipe connect 🐝…` command on stdout; the
//! consumer ([`connect`]) redeems the ticket and writes the stream to stdout.
//! The ticket is a bearer capability (a
//! random secret) carrying the producer's address + the swarm's discovery
//! config, so the consumer needs nothing but the ticket.

use std::time::Duration;

use anyhow::{Context, Result};
use iroh::Endpoint;

use crate::protocol::swarm::{LookupOpts, Swarm};

mod consume;
mod produce;
mod progress;
mod ticket;

pub(crate) use consume::connect;
pub(crate) use produce::listen;

/// ALPN for the pipe protocol — a raw bidirectional QUIC stream, distinct from
/// the gossip overlay's `GOSSIP_ALPN`.
pub(crate) const PIPE_ALPN: &[u8] = b"agent-habilis-swarm/pipe/1";

/// Length of the bearer-capability secret carried in a pipe ticket.
pub(crate) const SECRET_LEN: usize = 32;

/// Resolve a `--swarm` id to its discovery config (`None` ⇒ a public default),
/// so a pipe traverses the network the way that swarm's members do.
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
/// over the network), and stderr stays errors-only.
///
/// Human (default) mirrors `create`'s `others can join with: …` hint: a bee
/// status line + the command bold-blue on a terminal (plain when piped). `json`
/// is direct for machines — just the bare command, no status/colors.
fn announce(json: bool, status: &str, command: &str) {
    tracing::info!("{status}");
    if json {
        println!("{command}");
        return;
    }
    let (open, close) = if crate::output::stdout_color() {
        (crate::output::style::BLUE, crate::output::style::RESET)
    } else {
        ("", "")
    };
    println!("🐝 {status}");
    println!("other peer can connect with: {open}{command}{close}");
}

/// Emit a producer lifecycle line (`connected` / `transferring…` / `finished`) to
/// stdout in human mode; suppressed under `--output json` (machines watch the exit
/// code + the stream). Always logged. stderr stays errors-only.
fn stage(narrate: bool, msg: &str) {
    tracing::info!("{msg}");
    if narrate {
        println!("🐝 {msg}");
    }
}

#[cfg(test)]
mod tests {
    use crate::protocol::swarm::LookupOpts;

    /// Build a consumer endpoint, redeem the ticket once, and close it — the test
    /// stand-in for the CLI `connect`, which instead `process::exit`s on success
    /// (skipping the slow endpoint teardown).
    async fn fetch_once(
        ticket: &PipeTicket,
        sink: &mut Vec<u8>,
        throttle: Option<u64>,
    ) -> anyhow::Result<()> {
        let endpoint = crate::lookup::build_participant_endpoint(&ticket.lookups).await?;
        let result = super::consume::transfer(&endpoint, ticket, sink, throttle).await;
        endpoint.close().await;
        result
    }

    /// Run a producer and consumer over two loopback endpoints and return what
    /// the consumer received. The producer serves `data` once with the given
    /// `total` length header; `throttle` caps both sides' throughput (bytes/sec)
    /// when set. The bytes flow over a real direct QUIC connection (no gossip).
    async fn round_trip(data: &[u8], total: Option<u64>, throttle: Option<u64>) -> Vec<u8> {
        let (endpoint, ticket, secret) = super::produce::bind(LookupOpts::loopback())
            .await
            .expect("bind producer");
        let payload = data.to_vec();
        let server = tokio::spawn(async move {
            // `&[u8]` is a `tokio::io::AsyncRead`.
            let mut reader: &[u8] = &payload;
            let result =
                super::produce::serve(&endpoint, &secret, &mut reader, total, throttle, false)
                    .await;
            // The CLI `listen` `process::exit`s here, which closes the socket and
            // lets the consumer's `send.stopped()` resolve; the test must close
            // the endpoint explicitly so that wait doesn't hit the idle timeout.
            endpoint.close().await;
            result
        });

        let mut sink: Vec<u8> = Vec::new();
        fetch_once(&ticket, &mut sink, throttle)
            .await
            .expect("consumer fetch");
        server.await.expect("join server").expect("serve");
        sink
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn round_trips_known_length_payload() {
        let data: Vec<u8> = (0u8..=255).cycle().take(70_000).collect();
        let total = Some(u64::try_from(data.len()).unwrap());
        assert_eq!(round_trip(&data, total, None).await, data);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn round_trips_unknown_length_payload() {
        // A `tail -f`-style stream: no length header, data still byte-exact.
        let data: Vec<u8> = (0u8..=255).cycle().take(70_000).collect();
        assert_eq!(round_trip(&data, None, None).await, data);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn round_trips_empty_stream() {
        assert!(round_trip(b"", Some(0), None).await.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn round_trips_under_throttle() {
        // A high cap (10 MB/s) exercises the throttled copy path byte-exactly
        // without slowing the suite (70 KB ⇒ ~7 ms of pacing).
        let data: Vec<u8> = (0u8..=255).cycle().take(70_000).collect();
        let total = Some(u64::try_from(data.len()).unwrap());
        assert_eq!(round_trip(&data, total, Some(10_000_000)).await, data);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wrong_secret_is_rejected_then_right_one_succeeds() {
        let (endpoint, ticket, secret) = super::produce::bind(LookupOpts::loopback())
            .await
            .expect("bind producer");
        let server = tokio::spawn(async move {
            let mut reader: &[u8] = b"top secret";
            super::produce::serve(&endpoint, &secret, &mut reader, Some(10), None, false).await
        });

        // An impostor with the right address but the wrong secret is refused.
        let mut bad = ticket;
        let good_secret = bad.secret;
        bad.secret = [0u8; super::SECRET_LEN];
        let mut sink = Vec::new();
        assert!(
            fetch_once(&bad, &mut sink, None).await.is_err(),
            "a bad secret must be rejected"
        );

        // The real ticket still works (the producer kept serving).
        bad.secret = good_secret;
        let mut good_sink = Vec::new();
        fetch_once(&bad, &mut good_sink, None)
            .await
            .expect("good secret succeeds");
        server.await.expect("join").expect("serve");
        assert_eq!(good_sink, b"top secret");
    }

    // ── live-follow (`pipe listen --follow`) ──────────────────────────────

    use super::ticket::PipeTicket;
    use iroh::endpoint::{Connection, RecvStream, SendStream};
    use tokio::io::AsyncWriteExt;

    /// Run a follow-mode producer over a loopback endpoint, reading its live
    /// source from `reader` (the test owns the write half via `duplex`). Returns
    /// the ticket so a consumer can attach; the task runs until the source ends.
    async fn follow_producer(
        reader: tokio::io::DuplexStream,
    ) -> (PipeTicket, tokio::task::JoinHandle<()>) {
        let (endpoint, mut ticket, secret) = super::produce::bind(LookupOpts::loopback())
            .await
            .expect("bind producer");
        ticket.follow = true;
        let handle = tokio::spawn(async move {
            let mut reader = reader;
            let _ =
                super::produce::serve_follow(&endpoint, &secret, &mut reader, None, false).await;
            endpoint.close().await;
        });
        (ticket, handle)
    }

    /// Attach a raw follow-mode consumer: dial, open the bi-stream, present the
    /// secret, and read the 8-byte length header. `Err` if the producer closed
    /// the connection before the header (a pre-data drop). The returned
    /// endpoint/connection must be kept alive for the streams to stay usable.
    async fn attach_follow(
        ticket: &PipeTicket,
    ) -> anyhow::Result<(iroh::Endpoint, Connection, SendStream, RecvStream)> {
        let endpoint = crate::lookup::build_participant_endpoint(&ticket.lookups).await?;
        crate::lookup::add_peer_addr(&endpoint, ticket.addr.clone())?;
        let conn = endpoint
            .connect(ticket.addr.clone(), super::PIPE_ALPN)
            .await
            .map_err(|error| anyhow::anyhow!("connect failed: {error}"))?;
        let (mut send, mut recv) = conn.open_bi().await?;
        send.write_all(&ticket.secret).await?;
        let mut header = [0u8; 8];
        recv.read_exact(&mut header).await?;
        Ok((endpoint, conn, send, recv))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn follow_round_trips_then_exits_on_eof() {
        let (mut source, reader) = tokio::io::duplex(64 * 1024);
        let (ticket, producer) = follow_producer(reader).await;

        // Drive the real consumer path (transfer_follow) into a sink.
        let consumer = tokio::spawn(async move {
            let endpoint = crate::lookup::build_participant_endpoint(&ticket.lookups)
                .await
                .expect("consumer endpoint");
            let mut sink: Vec<u8> = Vec::new();
            super::consume::transfer_follow(&endpoint, &ticket, &mut sink, None)
                .await
                .expect("transfer_follow");
            endpoint.close().await;
            sink
        });

        // Pre-attach bytes are buffered (the producer reads only once a consumer
        // is attached), so the payload + EOF are delivered whenever the consumer
        // attaches — deterministic, no ordering sleep needed.
        source.write_all(b"live-payload").await.expect("write");
        drop(source); // EOF → producer FINs the consumer

        let sink = consumer.await.expect("consumer task");
        assert_eq!(sink, b"live-payload");
        producer.await.expect("producer task");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn follow_second_consumer_preempts_the_first() {
        let (mut source, reader) = tokio::io::duplex(64 * 1024);
        let (ticket, producer) = follow_producer(reader).await;

        // A attaches and is receiving.
        let (_endpoint_a, _conn_a, _send_a, mut recv_a) = attach_follow(&ticket).await.expect("A");
        source.write_all(b"aaaa").await.expect("write a");
        let mut got_a = [0u8; 4];
        recv_a.read_exact(&mut got_a).await.expect("A receives");
        assert_eq!(&got_a, b"aaaa");

        // B attaches with the same ticket — it preempts A and takes the slot
        // (instant reconnect; the single live slot is "latest connect wins").
        let (_endpoint_b, _conn_b, _send_b, mut recv_b) =
            attach_follow(&ticket).await.expect("B preempts A");

        // A is cut off — its next read resolves to an error, not live data.
        let a_cut = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recv_a.read_exact(&mut [0u8; 1]),
        )
        .await
        .expect("A's read must resolve, not hang")
        .is_err();
        assert!(a_cut, "A must be cut off once B preempts it");

        // B now receives the live stream.
        source.write_all(b"bbbb").await.expect("write b");
        let mut got_b = [0u8; 4];
        recv_b.read_exact(&mut got_b).await.expect("B receives");
        assert_eq!(&got_b, b"bbbb");

        drop(source);
        producer.await.expect("producer task");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn follow_drop_then_replacement_receives_tail() {
        let (mut source, reader) = tokio::io::duplex(64 * 1024);
        let (ticket, producer) = follow_producer(reader).await;

        // A attaches and receives the first chunk.
        let (endpoint_a, conn_a, send_a, mut recv_a) = attach_follow(&ticket).await.expect("A");
        source.write_all(b"chunk1").await.expect("write1");
        let mut got1 = [0u8; 6];
        recv_a.read_exact(&mut got1).await.expect("A gets chunk1");
        assert_eq!(&got1, b"chunk1");

        // A's process dies — drop everything it holds.
        drop((recv_a, send_a, conn_a, endpoint_a));

        // B reconnects (re-running `connect`); "latest connect wins" preempts the
        // zombie A still in the slot, so B attaches on the first try.
        let (_eb, _cb, _sb, mut recv_b) = attach_follow(&ticket).await.expect("B reconnects");

        // B gets the live tail from here on — chunk3, never the earlier chunk1.
        source.write_all(b"chunk3").await.expect("write3");
        let mut got3 = [0u8; 6];
        recv_b.read_exact(&mut got3).await.expect("B gets chunk3");
        assert_eq!(&got3, b"chunk3");

        drop(source);
        producer.await.expect("producer task");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn follow_delivers_buffered_backlog_then_live() {
        let (mut source, reader) = tokio::io::duplex(64 * 1024);
        let (ticket, producer) = follow_producer(reader).await;

        // The producer reads ONLY while a consumer is attached, so bytes written
        // before anyone attaches sit in the OS pipe buffer (not discarded). The
        // first consumer drains that backlog, then follows live.
        source.write_all(b"first").await.expect("backlog");

        let (_e, _c, _s, mut recv) = attach_follow(&ticket).await.expect("attach");
        source.write_all(b"second").await.expect("live");
        let mut got = [0u8; 11];
        recv.read_exact(&mut got).await.expect("backlog then live");
        assert_eq!(&got, b"firstsecond");

        drop(source);
        producer.await.expect("producer task");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn follow_idle_drop_frees_slot_via_closed() {
        let (mut source, reader) = tokio::io::duplex(64 * 1024);
        let (ticket, producer) = follow_producer(reader).await;

        // A attaches, then leaves CLEANLY while the source is idle (no data flows),
        // so the only thing that can free the slot is the producer's `closed()`
        // arm — not a write error. `endpoint.close()` AWAITS the CONNECTION_CLOSE
        // flush, so the producer reliably sees it (a bare `conn.close()` + drop
        // can race the driver teardown and lose the frame).
        let (endpoint_a, _conn_a, _send_a, _recv_a) = attach_follow(&ticket).await.expect("A");
        endpoint_a.close().await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // With the slot freed by `closed()`, data written now is buffered for the
        // next consumer rather than consumed by the zombie. If `closed()` failed
        // to free the slot, this byte is lost to the dead A and B's read hangs —
        // the timeout turns that regression into a clean failure.
        source.write_all(b"after-idle-drop").await.expect("write");
        let (_e, _c, _s, mut recv_b) = attach_follow(&ticket).await.expect("B");
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut got = [0u8; 15];
            recv_b.read_exact(&mut got).await.expect("B receives");
            got
        })
        .await
        .expect("closed() must free the slot so B receives the buffered data");
        assert_eq!(&got, b"after-idle-drop");

        drop(source);
        producer.await.expect("producer task");
    }
}
