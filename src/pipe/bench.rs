//! `pipe listen-bench` / `pipe connect-bench` — a throughput + latency
//! benchmark over a direct pipe connection. Data flows **consumer →
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
use crate::util::output::status;

use super::ticket::PipeTicket;

/// Bulk-payload chunk size for the throughput phase (content is never
/// verified, so a single static buffer is fine — no per-chunk generation
/// cost skewing the measured rate).
const CHUNK: usize = 64 * 1024;

/// The test-sizing knob for `pipe connect-bench --budget`: either run for a
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

/// Serve one `pipe connect-bench` run: bind, announce the ticket, wait for
/// the first peer to authenticate (retrying past a bad handshake — that's a
/// rejected impostor, not a real run), then run the benchmark and quit. A
/// single-shot lifetime, same as `listen`: a fresh `listen-bench` per
/// benchmark keeps runs from skewing each other's throughput numbers, and
/// gives a clean exit code to script against.
///
/// # Errors
/// Endpoint bind / discovery-config parse failures, or the benchmark itself
/// failing (a dropped connection, a malformed plan) — the caller then exits
/// non-zero.
pub(crate) async fn listen_bench(swarm: Option<&str>, json: bool) -> Result<()> {
    let lookups = super::swarm_lookups(swarm)?;
    let (endpoint, mut ticket, secret) = super::produce::bind(lookups).await?;
    ticket.bench = true;
    super::announce(
        json,
        "Waiting",
        "for a peer to benchmark",
        &format!("ahsw pipe connect-bench {}", ticket.encode()),
    );
    let narrate = !json;
    let (conn, send, recv) = loop {
        let Some(incoming) = endpoint.accept().await else {
            bail!("endpoint closed before a peer connected");
        };
        match super::produce::authenticate(incoming, &secret).await {
            Ok(triple) => break triple,
            Err(error) => tracing::debug!(%error, "bench handshake failed; awaiting another"),
        }
    };
    let result = serve_one(conn, send, recv, narrate).await;
    match result {
        // The consumer has its report; skip the multi-second `endpoint.close()`
        // teardown (relay/DHT/mDNS), same as `listen`'s and `connect`'s exit.
        Ok(()) => std::process::exit(0),
        Err(error) => {
            endpoint.close().await;
            Err(error)
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
    recv.read_exact(&mut header)
        .await
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
        format!("{value} bytes")
    } else {
        format!("{value}s")
    }
}

/// Options for `pipe connect-bench`.
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
pub(crate) async fn connect_bench(ticket: &str, opts: BenchOpts, json: bool) -> Result<()> {
    let ticket = PipeTicket::decode(ticket)?;
    if !ticket.bench {
        bail!(
            "this ticket is for `pipe connect`, not `pipe connect-bench` — \
             run `pipe listen-bench` on the producer to get a bench ticket"
        );
    }
    let endpoint = build_participant_endpoint(&ticket.lookups).await?;
    let result = run(&endpoint, &ticket, opts).await;
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

async fn run(endpoint: &Endpoint, ticket: &PipeTicket, opts: BenchOpts) -> Result<BenchReport> {
    let (conn, mut send, mut recv) =
        super::consume::dial_and_authenticate(endpoint, ticket).await?;

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

    /// Run one full bench protocol exchange over loopback endpoints: a
    /// producer task driving `serve_one` directly (the test stand-in for
    /// `listen_bench`'s accept loop, same reasoning as `pipe::tests`'
    /// `round_trip` driving `produce`/`consume` directly instead of the CLI
    /// `listen`/`connect`) against the real `run()` the CLI `connect_bench`
    /// calls.
    async fn bench_round_trip(pings: u32, budget: BenchBudget) -> BenchReport {
        let (endpoint, ticket, secret) = super::super::produce::bind(LookupOpts::loopback())
            .await
            .expect("bind producer");
        let server = tokio::spawn(async move {
            let incoming = endpoint.accept().await.expect("incoming connection");
            let (conn, send, recv) = super::super::produce::authenticate(incoming, &secret)
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
        let report = super::run(&consumer_endpoint, &ticket, BenchOpts { budget, pings })
            .await
            .expect("run bench");
        consumer_endpoint.close().await;
        server.await.expect("server task");
        report
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
        let (endpoint, mut ticket, _secret) = super::super::produce::bind(LookupOpts::loopback())
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
        )
        .await
        .expect_err("a plain ticket must be refused by connect-bench");
        assert!(error.to_string().contains("pipe connect-bench"));
        endpoint.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn plain_connect_rejects_a_bench_ticket() {
        let (endpoint, mut ticket, _secret) = super::super::produce::bind(LookupOpts::loopback())
            .await
            .expect("bind producer");
        ticket.bench = true;
        let error = super::super::consume::connect(&ticket.encode(), None)
            .await
            .expect_err("a bench ticket must be refused by plain connect");
        assert!(error.to_string().contains("pipe connect"));
        endpoint.close().await;
    }
}
