//! `pipe bench` (producer) / `pipe bench 🐝…` (consumer) — a throughput +
//! latency benchmark over a direct pipe connection. Data flows **consumer →
//! producer**, the opposite direction from `listen`/`connect`: the consumer
//! drives and times the whole run, the producer just counts what it actually
//! received (the receiver is the ground truth for real delivered
//! throughput, same principle `serve`'s reverse report channel already
//! uses for the plain byte-stream).
//!
//! Wire protocol, after the shared secret handshake
//! ([`super::consume::dial_and_authenticate`] / [`super::produce::authenticate`]):
//!  1. plan header (consumer → producer, once): `[pings: u32 LE][budget_kind: u8][budget_value: u64 LE]`
//!  2. `pings` sequential ping/pong round-trips: consumer writes `[nonce: u64 LE]`, producer echoes it
//!  3. bulk payload (consumer → producer) until the budget is spent, then the consumer finishes the stream
//!  4. stats frame (producer → consumer, once): `[bytes_received: u64 LE][elapsed_micros: u64 LE]`
#![allow(
    clippy::cast_precision_loss,
    reason = "throughput/latency report math; f64 precision is irrelevant for a human summary"
)]

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use iroh::Endpoint;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use tokio::io::AsyncReadExt;

use crate::lookup::build_participant_endpoint;
use crate::protocol::swarm::{LookupSet, resolve_transfer_lookups};
use crate::util::output::status;

use super::ticket::PipeTicket;

/// Bulk-payload chunk size for the throughput phase (content is never
/// verified, so a single static buffer is fine — no per-chunk generation
/// cost skewing the measured rate).
const CHUNK: usize = 64 * 1024;

/// How long the producer waits for the consumer's plan header before giving up
/// on a run. The consumer sends it immediately after the secret handshake, so
/// this only ever trips on a peer that connects and then goes silent — which in
/// `--serve` mode would otherwise wedge the sequential accept loop forever.
const PLAN_TIMEOUT: Duration = Duration::from_secs(10);

/// The test-sizing knob for `pipe bench --budget`: either run for a
/// wall-clock duration or until a byte count is sent, whichever the value's
/// suffix picked (`10s`/`2m`/`1h` vs `500b`/`100kb`/`50mb`/`2gb`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BenchBudget {
    Duration(Duration),
    Bytes(u64),
}

impl Default for BenchBudget {
    fn default() -> Self {
        Self::Duration(Duration::from_secs(10))
    }
}

impl BenchBudget {
    const KIND_DURATION: u8 = 0;
    const KIND_BYTES: u8 = 1;

    fn to_header(self) -> (u8, u64) {
        match self {
            Self::Duration(duration) => (Self::KIND_DURATION, duration.as_secs()),
            Self::Bytes(bytes) => (Self::KIND_BYTES, bytes),
        }
    }
}

/// Parse a `--budget` value: an explicit-suffix duration (`10s`, `2m`, `1h`)
/// or an explicit-suffix byte count (`500b`, `100kb`, `50mb`, `2gb`,
/// 1024-based). No bare numbers — an explicit unit avoids the `m` = minutes
/// vs `m` = mebibytes ambiguity a shared suffix set would have.
pub(crate) fn parse_budget(raw: &str) -> Result<BenchBudget, String> {
    let raw = raw.trim();
    if let Some(digits) = raw.strip_suffix('h') {
        return parse_budget_number(digits, raw)
            .map(|value| BenchBudget::Duration(Duration::from_secs(value * 3600)));
    }
    if let Some(digits) = raw.strip_suffix("gb") {
        return parse_budget_number(digits, raw).map(|value| BenchBudget::Bytes(value << 30));
    }
    if let Some(digits) = raw.strip_suffix("mb") {
        return parse_budget_number(digits, raw).map(|value| BenchBudget::Bytes(value << 20));
    }
    if let Some(digits) = raw.strip_suffix("kb") {
        return parse_budget_number(digits, raw).map(|value| BenchBudget::Bytes(value << 10));
    }
    if let Some(digits) = raw.strip_suffix('b') {
        return parse_budget_number(digits, raw).map(BenchBudget::Bytes);
    }
    if let Some(digits) = raw.strip_suffix('m') {
        return parse_budget_number(digits, raw)
            .map(|value| BenchBudget::Duration(Duration::from_secs(value * 60)));
    }
    if let Some(digits) = raw.strip_suffix('s') {
        return parse_budget_number(digits, raw)
            .map(|value| BenchBudget::Duration(Duration::from_secs(value)));
    }
    Err(format!(
        "invalid budget `{raw}` — use an explicit unit: 10s, 2m, 1h (duration) or 500b, 100kb, 50mb, 2gb (bytes)"
    ))
}

fn parse_budget_number(digits: &str, raw: &str) -> Result<u64, String> {
    let value: u64 = digits
        .parse()
        .map_err(|_| format!("invalid budget `{raw}` — use e.g. 10s, 2m, 100kb, 50mb"))?;
    if value == 0 {
        return Err(format!("budget `{raw}` must be greater than 0"));
    }
    Ok(value)
}

/// Accept and authenticate one peer, retrying past a bad handshake (a rejected
/// impostor, not a real run). Shared by both the single-shot and `--serve`
/// paths of [`listen_bench`].
async fn accept_authenticated(
    endpoint: &Endpoint,
    auth: &crate::protocol::crypto::TicketAuth,
) -> Result<(Connection, SendStream, RecvStream)> {
    loop {
        let Some(incoming) = endpoint.accept().await else {
            bail!("endpoint closed before a peer connected");
        };
        match super::produce::authenticate(incoming, auth).await {
            Ok(triple) => return Ok(triple),
            Err(error) => tracing::debug!(%error, "bench handshake failed; awaiting another"),
        }
    }
}

/// Serve `pipe bench` runs: bind, announce the ticket, then serve one benchmark
/// against the first peer that authenticates. Single-shot by default (a fresh
/// producer per run keeps concurrent benchmarks from skewing each other's
/// numbers, and gives a clean exit code to script against); with `serve`, stay
/// up and serve one run per peer sequentially until the process is killed — the
/// ticket stays valid for the producer's whole lifetime, so a consumer
/// reconnects by re-running `pipe bench <ticket>`.
///
/// # Errors
/// Endpoint bind / discovery-config parse failures, or (single-shot only) the
/// benchmark itself failing — the caller then exits non-zero. In `serve` mode a
/// failed run is one bad consumer, not fatal; only the endpoint closing ends it.
pub(crate) async fn listen_bench(
    swarm: Option<&str>,
    flags: LookupSet,
    serve: bool,
    json: bool,
    password: Option<crate::protocol::crypto::Password>,
) -> Result<()> {
    let lookups = resolve_transfer_lookups(swarm, flags)?;
    let (endpoint, mut ticket, auth) = super::produce::bind(lookups, password.as_ref()).await?;
    ticket.bench = true;
    super::announce(
        json,
        "Waiting",
        if serve {
            "for peers to benchmark (staying up)"
        } else {
            "for a peer to benchmark"
        },
        &format!("ahsw pipe bench {}", ticket.encode()),
    );
    let narrate = !json;
    loop {
        let (conn, send, recv) = accept_authenticated(&endpoint, &auth).await?;
        let result = serve_one(conn, send, recv, narrate).await;
        if !serve {
            return match result {
                // The consumer has its report; skip the multi-second
                // `endpoint.close()` teardown (relay/DHT/mDNS), same as
                // `listen`'s and `connect`'s exit.
                Ok(()) => std::process::exit(0),
                Err(error) => {
                    endpoint.close().await;
                    Err(error)
                }
            };
        }
        if let Err(error) = result {
            tracing::warn!(%error, "bench run ended early; awaiting another peer");
        }
        if narrate {
            status("Waiting", "for the next peer to benchmark");
        }
    }
}

/// Run one benchmark: read the consumer's plan, echo its pings, discard the
/// bulk payload while counting bytes and elapsed time, then report both back.
async fn serve_one(
    conn: Connection,
    mut send: SendStream,
    mut recv: RecvStream,
    narrate: bool,
) -> Result<()> {
    let mut header = [0u8; 13];
    tokio::time::timeout(PLAN_TIMEOUT, recv.read_exact(&mut header))
        .await
        .context("timed out waiting for the bench plan (peer connected but sent nothing)")?
        .context("reading the bench plan failed")?;
    let pings = u32::from_le_bytes(header[0..4].try_into().expect("4 bytes"));
    let budget_kind = header[4];
    let budget_value = u64::from_le_bytes(header[5..13].try_into().expect("8 bytes"));
    if narrate {
        status(
            "Serving",
            &format!(
                "{pings} pings, budget {}",
                describe_budget(budget_kind, budget_value)
            ),
        );
    }

    let mut nonce = [0u8; 8];
    for _ in 0..pings {
        recv.read_exact(&mut nonce)
            .await
            .context("reading a ping failed")?;
        send.write_all(&nonce)
            .await
            .context("sending a pong failed")?;
    }

    let mut buf = vec![0u8; CHUNK];
    let mut bytes_received: u64 = 0;
    let started = Instant::now();
    loop {
        // The inherent `RecvStream::read` returns QUIC-flavored
        // `Result<Option<usize>>`; force the `AsyncReadExt` trait method
        // instead, which gives the usual `Ok(0)` = EOF (same disambiguation
        // `consume.rs`'s `transfer_follow` needs for the same reason).
        let read = AsyncReadExt::read(&mut recv, &mut buf)
            .await
            .context("reading the bulk payload failed")?;
        match read {
            0 => break,
            read => bytes_received += u64::try_from(read).unwrap_or(0),
        }
    }
    let elapsed = started.elapsed();

    send.write_all(&bytes_received.to_le_bytes())
        .await
        .context("sending the stats frame failed")?;
    send.write_all(
        &u64::try_from(elapsed.as_micros())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    )
    .await
    .context("sending the stats frame failed")?;
    send.finish().context("finishing the stats stream failed")?;
    let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
    conn.close(0u32.into(), b"done");
    if narrate {
        status(
            "Received",
            &format!(
                "{:.2} MB in {:.2}s",
                mb(bytes_received),
                elapsed.as_secs_f64()
            ),
        );
        status(
            "Throughput",
            &format!("{:.2} MB/s", mb_per_sec(bytes_received, elapsed)),
        );
    }
    Ok(())
}

/// Bytes as MB (1024-based), shared by the producer's own report
/// ([`serve_one`]) and the consumer's ([`print_report`]) so both sides render
/// the identical figures identically.
fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// MB/s over `elapsed`, `0.0` for a non-positive duration (avoids a NaN/inf
/// from a near-instant transfer).
fn mb_per_sec(bytes: u64, elapsed: Duration) -> f64 {
    let secs = elapsed.as_secs_f64();
    if secs <= 0.0 { 0.0 } else { mb(bytes) / secs }
}

fn describe_budget(kind: u8, value: u64) -> String {
    if kind == BenchBudget::KIND_BYTES {
        describe_bytes(value)
    } else {
        format!("{value}s")
    }
}

/// A byte count as a human-readable size in the largest 1024-based unit that
/// keeps the number ≥ 1 (`1.00 GB`, `20.00 MB`, `512.00 KB`, `500 B`) — the
/// same units `--budget` accepts, so a `1gb` budget reads back as `1.00 GB`.
fn describe_bytes(bytes: u64) -> String {
    const KB: u64 = 1 << 10;
    const MB: u64 = 1 << 20;
    const GB: u64 = 1 << 30;
    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Options for the consumer side of `pipe bench`.
pub(crate) struct BenchOpts {
    pub budget: BenchBudget,
    pub pings: u32,
}

/// A completed benchmark's measurements — round-trip latencies from the
/// ping/pong phase, and the throughput phase's byte count + elapsed time as
/// reported by the producer (the ground truth for real delivered bytes).
pub(crate) struct BenchReport {
    rtts: Vec<Duration>,
    bytes_sent: u64,
    bytes_received: u64,
    producer_elapsed: Duration,
}

impl BenchReport {
    fn rtt_min(&self) -> Duration {
        self.rtts.iter().copied().min().unwrap_or_default()
    }

    fn rtt_max(&self) -> Duration {
        self.rtts.iter().copied().max().unwrap_or_default()
    }

    fn rtt_avg(&self) -> Duration {
        if self.rtts.is_empty() {
            return Duration::default();
        }
        self.rtts.iter().sum::<Duration>() / u32::try_from(self.rtts.len()).unwrap_or(1)
    }

    fn throughput_bytes_per_sec(&self) -> f64 {
        let secs = self.producer_elapsed.as_secs_f64();
        if secs <= 0.0 {
            0.0
        } else {
            self.bytes_received as f64 / secs
        }
    }
}

/// Redeem a bench ticket and run the whole benchmark: dial, send the plan,
/// run the ping/pong latency phase, stream the throughput phase, then print
/// the report built from the producer's own byte count.
///
/// # Errors
/// A malformed or non-bench ticket, an unreachable producer, a nonce
/// mismatch (the producer is misbehaving), or a dropped connection.
pub(crate) async fn connect_bench(
    ticket: &str,
    opts: BenchOpts,
    json: bool,
    password: Option<crate::protocol::crypto::Password>,
) -> Result<()> {
    let ticket = PipeTicket::decode(ticket)?;
    if !ticket.bench {
        bail!(
            "this ticket is for `pipe connect`, not `pipe bench` — \
             run `pipe bench` on the producer to mint a bench ticket"
        );
    }
    let auth = super::consume::ticket_auth(&ticket, password.as_ref())?;
    let endpoint = build_participant_endpoint(&ticket.lookups).await?;
    let result = run(&endpoint, &ticket, &auth, opts).await;
    match result {
        Ok(report) => {
            print_report(&report, json);
            // Mirrors `connect`: the data is delivered and the producer
            // already got our `conn.close`, so skip the multi-second
            // `endpoint.close()` teardown (relay/DHT/mDNS).
            std::process::exit(0);
        }
        Err(error) => {
            endpoint.close().await;
            Err(error)
        }
    }
}

async fn run(
    endpoint: &Endpoint,
    ticket: &PipeTicket,
    auth: &crate::protocol::crypto::TicketAuth,
    opts: BenchOpts,
) -> Result<BenchReport> {
    let (conn, mut send, mut recv) =
        super::consume::dial_and_authenticate(endpoint, ticket, auth).await?;

    let (budget_kind, budget_value) = opts.budget.to_header();
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&opts.pings.to_le_bytes());
    header.push(budget_kind);
    header.extend_from_slice(&budget_value.to_le_bytes());
    send.write_all(&header)
        .await
        .context("sending the bench plan failed")?;

    let mut rtts = Vec::with_capacity(usize::try_from(opts.pings).unwrap_or(0));
    let mut nonce_buf = [0u8; 8];
    for i in 0..opts.pings {
        let nonce = u64::from(i);
        let started = Instant::now();
        send.write_all(&nonce.to_le_bytes())
            .await
            .context("sending a ping failed")?;
        recv.read_exact(&mut nonce_buf)
            .await
            .context("reading a pong failed")?;
        if u64::from_le_bytes(nonce_buf) != nonce {
            bail!("pong nonce mismatch — the producer is misbehaving");
        }
        rtts.push(started.elapsed());
    }

    let payload = vec![0xABu8; CHUNK];
    let deadline = match opts.budget {
        BenchBudget::Duration(duration) => Some(Instant::now() + duration),
        BenchBudget::Bytes(_) => None,
    };
    let target_bytes = match opts.budget {
        BenchBudget::Bytes(bytes) => Some(bytes),
        BenchBudget::Duration(_) => None,
    };
    let mut bytes_sent: u64 = 0;
    loop {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            break;
        }
        if target_bytes.is_some_and(|target| bytes_sent >= target) {
            break;
        }
        let chunk = match target_bytes {
            Some(target) => {
                let remaining = target.saturating_sub(bytes_sent);
                let len =
                    usize::try_from(remaining.min(payload.len() as u64)).unwrap_or(payload.len());
                &payload[..len]
            }
            None => &payload[..],
        };
        send.write_all(chunk)
            .await
            .context("sending the bench payload failed")?;
        bytes_sent += u64::try_from(chunk.len()).unwrap_or(0);
    }
    send.finish()
        .context("finishing the bench payload stream failed")?;

    let mut stats = [0u8; 16];
    recv.read_exact(&mut stats)
        .await
        .context("reading the producer's stats frame failed")?;
    let bytes_received = u64::from_le_bytes(stats[0..8].try_into().expect("8 bytes"));
    let producer_elapsed = Duration::from_micros(u64::from_le_bytes(
        stats[8..16].try_into().expect("8 bytes"),
    ));

    let _ = send.stopped().await;
    conn.close(0u32.into(), b"done");

    Ok(BenchReport {
        rtts,
        bytes_sent,
        bytes_received,
        producer_elapsed,
    })
}

fn print_report(report: &BenchReport, json: bool) {
    if json {
        let value = serde_json::json!({
            "pings": report.rtts.len(),
            "rtt_min_ms": report.rtt_min().as_secs_f64() * 1000.0,
            "rtt_avg_ms": report.rtt_avg().as_secs_f64() * 1000.0,
            "rtt_max_ms": report.rtt_max().as_secs_f64() * 1000.0,
            "bytes_sent": report.bytes_sent,
            "bytes_received": report.bytes_received,
            "elapsed_secs": report.producer_elapsed.as_secs_f64(),
            "throughput_bytes_per_sec": report.throughput_bytes_per_sec(),
        });
        println!("{value}");
        return;
    }
    status(
        "Latency",
        &format!(
            "{:.1}ms min · {:.1}ms avg · {:.1}ms max  ({} pings)",
            report.rtt_min().as_secs_f64() * 1000.0,
            report.rtt_avg().as_secs_f64() * 1000.0,
            report.rtt_max().as_secs_f64() * 1000.0,
            report.rtts.len(),
        ),
    );
    status(
        "Received",
        &format!(
            "{:.2} MB in {:.2}s (as measured by the producer)",
            mb(report.bytes_received),
            report.producer_elapsed.as_secs_f64(),
        ),
    );
    status(
        "Throughput",
        &format!(
            "{:.2} MB/s",
            mb_per_sec(report.bytes_received, report.producer_elapsed)
        ),
    );
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{BenchBudget, BenchOpts, BenchReport, parse_budget};
    use crate::lookup::build_participant_endpoint;
    use crate::protocol::swarm::LookupOpts;

    #[test]
    fn parses_duration_budgets() {
        assert_eq!(
            parse_budget("10s"),
            Ok(BenchBudget::Duration(Duration::from_secs(10)))
        );
        assert_eq!(
            parse_budget("2m"),
            Ok(BenchBudget::Duration(Duration::from_mins(2)))
        );
        assert_eq!(
            parse_budget("1h"),
            Ok(BenchBudget::Duration(Duration::from_hours(1)))
        );
    }

    #[test]
    fn parses_byte_budgets() {
        assert_eq!(parse_budget("500b"), Ok(BenchBudget::Bytes(500)));
        assert_eq!(parse_budget("100kb"), Ok(BenchBudget::Bytes(100 * 1024)));
        assert_eq!(
            parse_budget("50mb"),
            Ok(BenchBudget::Bytes(50 * 1024 * 1024))
        );
        assert_eq!(parse_budget("2gb"), Ok(BenchBudget::Bytes(2 << 30)));
    }

    #[test]
    fn rejects_bare_numbers_and_zero() {
        assert!(parse_budget("10").is_err());
        assert!(parse_budget("0s").is_err());
        assert!(parse_budget("0mb").is_err());
        assert!(parse_budget("abc").is_err());
    }

    #[test]
    fn describes_bytes_in_the_largest_1024_unit() {
        assert_eq!(super::describe_bytes(1 << 30), "1.00 GB");
        assert_eq!(super::describe_bytes(20 * (1 << 20)), "20.00 MB");
        assert_eq!(super::describe_bytes(512 * (1 << 10)), "512.00 KB");
        assert_eq!(super::describe_bytes(500), "500 B");
    }

    /// Run one full bench protocol exchange over loopback endpoints: a
    /// producer task driving `serve_one` directly (the test stand-in for
    /// `listen_bench`'s accept loop, same reasoning as `pipe::tests`'
    /// `round_trip` driving `produce`/`consume` directly instead of the CLI
    /// `listen`/`connect`) against the real `run()` the CLI `connect_bench`
    /// calls.
    async fn bench_round_trip(pings: u32, budget: BenchBudget) -> BenchReport {
        let (endpoint, ticket, auth) = super::super::produce::bind(LookupOpts::loopback(), None)
            .await
            .expect("bind producer");
        let consumer_auth = auth.clone();
        let server = tokio::spawn(async move {
            let incoming = endpoint.accept().await.expect("incoming connection");
            let (conn, send, recv) = super::super::produce::authenticate(incoming, &auth)
                .await
                .expect("authenticate");
            super::serve_one(conn, send, recv, false)
                .await
                .expect("serve_one");
            endpoint.close().await;
        });

        let consumer_endpoint = build_participant_endpoint(&ticket.lookups)
            .await
            .expect("consumer endpoint");
        let report = super::run(
            &consumer_endpoint,
            &ticket,
            &consumer_auth,
            BenchOpts { budget, pings },
        )
        .await
        .expect("run bench");
        consumer_endpoint.close().await;
        server.await.expect("server task");
        report
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn serve_mode_serves_sequential_runs_on_one_endpoint() {
        // A `--serve` producer binds once and re-accepts: two consecutive
        // consumers over the SAME endpoint (the same ticket) must both get a
        // valid report, proving the re-accept loop after `serve_one` works.
        let (endpoint, ticket, auth) = super::super::produce::bind(LookupOpts::loopback(), None)
            .await
            .expect("bind producer");
        let consumer_auth = auth.clone();
        let server = tokio::spawn(async move {
            loop {
                let (conn, send, recv) = super::accept_authenticated(&endpoint, &auth)
                    .await
                    .expect("accept authenticated");
                super::serve_one(conn, send, recv, false)
                    .await
                    .expect("serve_one");
            }
        });

        for _ in 0..2 {
            let consumer_endpoint = build_participant_endpoint(&ticket.lookups)
                .await
                .expect("consumer endpoint");
            let report = super::run(
                &consumer_endpoint,
                &ticket,
                &consumer_auth,
                BenchOpts {
                    budget: BenchBudget::Bytes(200_003),
                    pings: 3,
                },
            )
            .await
            .expect("run bench");
            assert_eq!(report.bytes_received, 200_003);
            assert_eq!(report.rtts.len(), 3);
            consumer_endpoint.close().await;
        }

        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ping_pong_phase_measures_a_sane_loopback_rtt() {
        let report = bench_round_trip(10, BenchBudget::Bytes(0)).await;
        assert_eq!(report.rtts.len(), 10);
        // Loopback RTTs are sub-millisecond in practice; a generous bound
        // keeps this from flaking on a loaded CI box.
        assert!(
            report.rtt_max() < Duration::from_secs(2),
            "rtt_max suspiciously large: {:?}",
            report.rtt_max()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn duration_budget_stops_close_to_the_requested_time() {
        let budget = Duration::from_millis(300);
        let report = bench_round_trip(1, BenchBudget::Duration(budget)).await;
        // The producer's clock starts at its first read and stops at EOF, so
        // it undershoots the consumer's write-loop deadline slightly; allow
        // a generous window either side for CI scheduling jitter.
        assert!(
            report.producer_elapsed >= Duration::from_millis(150)
                && report.producer_elapsed <= Duration::from_secs(3),
            "producer_elapsed {:?} not close to the {budget:?} budget",
            report.producer_elapsed
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn byte_budget_stops_at_exactly_the_requested_size() {
        // Not a multiple of the internal chunk size, so this also exercises
        // the final partial chunk getting trimmed to the exact remainder.
        let report = bench_round_trip(1, BenchBudget::Bytes(200_003)).await;
        assert_eq!(report.bytes_sent, 200_003);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn producer_stats_match_what_the_consumer_actually_sent() {
        let report = bench_round_trip(1, BenchBudget::Bytes(500_000)).await;
        assert_eq!(report.bytes_received, report.bytes_sent);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connect_rejects_a_plain_ticket() {
        let (endpoint, mut ticket, _auth) =
            super::super::produce::bind(LookupOpts::loopback(), None)
                .await
                .expect("bind producer");
        ticket.bench = false;
        let error = super::connect_bench(
            &ticket.encode(),
            BenchOpts {
                budget: BenchBudget::default(),
                pings: 1,
            },
            false,
            None,
        )
        .await
        .expect_err("a plain ticket must be refused by `pipe bench`");
        assert!(error.to_string().contains("pipe bench"));
        endpoint.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn plain_connect_rejects_a_bench_ticket() {
        let (endpoint, mut ticket, _auth) =
            super::super::produce::bind(LookupOpts::loopback(), None)
                .await
                .expect("bind producer");
        ticket.bench = true;
        let error = super::super::consume::connect(&ticket.encode(), None, None)
            .await
            .expect_err("a bench ticket must be refused by plain connect");
        assert!(error.to_string().contains("pipe connect"));
        endpoint.close().await;
    }
}
