//! `ahsw pipe` — a P2P byte stream over a dedicated direct
//! QUIC connection, off the gossip log. The producer ([`listen`]) reads stdin
//! and prints the consumer's `ahs pipe connect 🐝…` command on stdout; the
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
mod tcp;
mod ticket;

pub(crate) use consume::connect;
pub(crate) use produce::listen;
pub(crate) use tcp::{connect_tcp, listen_tcp};

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
        ticket: &super::ticket::PipeTicket,
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
}
