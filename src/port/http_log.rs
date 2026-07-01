use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, ReadBuf};

/// Cap on a single candidate line (request/status line) before giving up on
/// it — real HTTP start-lines are well under this; a longer "line" means
/// either non-HTTP traffic or a body byte sequence that happened to contain
/// `\r\n`, neither of which we want to buffer without bound.
const MAX_LINE_LEN: usize = 8 * 1024;

const METHODS: [&str; 9] = [
    "GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS", "CONNECT", "TRACE",
];

/// A completed request/response pairing, ready to print as one access-log
/// line.
pub(crate) struct LogLine {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) status: u16,
    pub(crate) bytes: u64,
    pub(crate) latency: Duration,
}

struct PendingRequest {
    method: String,
    path: String,
    started: Instant,
}

struct OpenResponse {
    method: String,
    path: String,
    status: u16,
    latency: Duration,
    bytes: u64,
}

enum Gate {
    /// Not yet seen a full first line on the upstream (request) side.
    Unknown,
    Http,
    NotHttp,
}

/// Where the downstream scanner is within one response.
enum DownstreamState {
    /// Looking for the next status line. Correct as the steady state for
    /// chunked/close-delimited responses (a chunked body's terminator ends
    /// in `\r\n`, so the next status line starts cleanly); for
    /// `Content-Length` responses this is only entered once the declared
    /// body has been fully skipped — see `SkippingBody`.
    AwaitingStatusLine,
    /// Status line found; scanning header lines for `Content-Length` until
    /// the blank line that ends the header block.
    InHeaders { content_length: Option<u64> },
    /// A `Content-Length` was declared — consume exactly that many body
    /// bytes as opaque (never line-scanned) before resuming
    /// `AwaitingStatusLine`. Without this, a body with no trailing `\r\n`
    /// (the normal case) glues onto the next response's status line and
    /// permanently breaks detection for the rest of the connection.
    SkippingBody { remaining: u64 },
}

/// Pure, synchronous best-effort HTTP/1.x access-log state machine. Fed raw
/// bytes from each direction of a forwarded TCP connection; never alters
/// them, only observes. Self-disables for the connection's lifetime the
/// moment the first upstream line doesn't look like an HTTP request line —
/// so non-HTTP traffic (TLS, raw protocols, binary) costs one failed scan.
pub(crate) struct AccessLog {
    gate: Gate,
    request_buf: Vec<u8>,
    response_buf: Vec<u8>,
    pending: VecDeque<PendingRequest>,
    open: Option<OpenResponse>,
    downstream: DownstreamState,
}

impl AccessLog {
    pub(crate) fn new() -> Self {
        Self {
            gate: Gate::Unknown,
            request_buf: Vec::new(),
            response_buf: Vec::new(),
            pending: VecDeque::new(),
            open: None,
            downstream: DownstreamState::AwaitingStatusLine,
        }
    }

    fn disable(&mut self) {
        self.gate = Gate::NotHttp;
        self.request_buf.clear();
        self.response_buf.clear();
        self.pending.clear();
        self.open = None;
        self.downstream = DownstreamState::AwaitingStatusLine;
    }

    /// Feed bytes read from the local-client → producer direction.
    pub(crate) fn feed_upstream(&mut self, chunk: &[u8]) {
        if matches!(self.gate, Gate::NotHttp) {
            return;
        }
        self.request_buf.extend_from_slice(chunk);

        if matches!(self.gate, Gate::Unknown) {
            let Some(line) = take_line(&mut self.request_buf) else {
                if self.request_buf.len() > MAX_LINE_LEN {
                    self.disable();
                }
                return;
            };
            let Some((method, path)) = parse_request_line(&line) else {
                self.disable();
                return;
            };
            self.gate = Gate::Http;
            self.pending.push_back(PendingRequest {
                method,
                path,
                started: Instant::now(),
            });
        }

        while let Some(line) = take_line(&mut self.request_buf) {
            if let Some((method, path)) = parse_request_line(&line) {
                self.pending.push_back(PendingRequest {
                    method,
                    path,
                    started: Instant::now(),
                });
            }
        }
        if self.request_buf.len() > MAX_LINE_LEN {
            // A candidate line that never terminated — not a header we
            // care about; drop it and keep scanning forward.
            self.request_buf.clear();
        }
    }

    /// Feed bytes read from the producer → local-client direction. Returns
    /// any response that just completed (a new status line was found, or
    /// — see [`Self::finish`] for the final flush at connection close).
    pub(crate) fn feed_downstream(&mut self, chunk: &[u8]) -> Vec<LogLine> {
        if matches!(self.gate, Gate::NotHttp) {
            return Vec::new();
        }
        self.response_buf.extend_from_slice(chunk);

        let mut completed = Vec::new();
        loop {
            match &mut self.downstream {
                DownstreamState::SkippingBody { remaining } => {
                    let available = u64_len(&self.response_buf);
                    let consumed = available.min(*remaining);
                    let buf_len = self.response_buf.len();
                    self.response_buf
                        .drain(..usize::try_from(consumed).unwrap_or(buf_len));
                    *remaining -= consumed;
                    attribute(&mut self.open, consumed);
                    if *remaining > 0 {
                        break;
                    }
                    self.downstream = DownstreamState::AwaitingStatusLine;
                }
                DownstreamState::AwaitingStatusLine => {
                    let Some(line) = take_line(&mut self.response_buf) else {
                        break;
                    };
                    let drained = u64_len(&line) + 2; // the line itself plus its stripped `\r\n`
                    let Some(status) = parse_status_line(&line) else {
                        attribute(&mut self.open, drained);
                        continue;
                    };
                    let Some(request) = self.pending.pop_front() else {
                        attribute(&mut self.open, drained);
                        continue;
                    };
                    if let Some(previous) = self.open.take() {
                        completed.push(previous.into_log_line());
                    }
                    self.open = Some(OpenResponse {
                        method: request.method,
                        path: request.path,
                        status,
                        latency: request.started.elapsed(),
                        bytes: drained,
                    });
                    self.downstream = DownstreamState::InHeaders {
                        content_length: None,
                    };
                }
                DownstreamState::InHeaders { content_length } => {
                    let Some(line) = take_line(&mut self.response_buf) else {
                        break;
                    };
                    attribute(&mut self.open, u64_len(&line) + 2);
                    if line.is_empty() {
                        // Per HTTP/1.1, a response to HEAD or a 204/304 never
                        // has a body regardless of Content-Length — servers
                        // commonly send the header anyway (echoing what a GET
                        // would have returned). Skipping it as body would eat
                        // the next response's bytes off the same connection.
                        let no_body = self.open.as_ref().is_some_and(|open| {
                            open.method == "HEAD" || matches!(open.status, 204 | 304)
                        });
                        self.downstream = if no_body {
                            DownstreamState::AwaitingStatusLine
                        } else {
                            match *content_length {
                                Some(remaining) => DownstreamState::SkippingBody { remaining },
                                None => DownstreamState::AwaitingStatusLine,
                            }
                        };
                    } else if let Some(declared) = parse_content_length(&line) {
                        *content_length = Some(declared);
                    }
                }
            }
        }
        if self.response_buf.len() > MAX_LINE_LEN {
            self.response_buf.clear();
        }
        completed
    }

    /// Flush the response still in flight when the connection ends.
    pub(crate) fn finish(&mut self) -> Option<LogLine> {
        self.open.take().map(OpenResponse::into_log_line)
    }
}

impl OpenResponse {
    fn into_log_line(self) -> LogLine {
        LogLine {
            method: self.method,
            path: self.path,
            status: self.status,
            bytes: self.bytes,
            latency: self.latency,
        }
    }
}

fn u64_len(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).unwrap_or(u64::MAX)
}

/// Add `bytes` to the currently-open response's running size, if any —
/// bytes drained from `response_buf` while nothing is open (before the
/// first status line of the connection has been seen) have nothing to
/// attribute to and are simply dropped.
fn attribute(open: &mut Option<OpenResponse>, bytes: u64) {
    if let Some(open) = open {
        open.bytes = open.bytes.saturating_add(bytes);
    }
}

/// Pull one complete `\r\n`-terminated line off the front of `buf`, if one
/// is present; the line (without the trailing `\r\n`) is removed from `buf`.
fn take_line(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    let position = buf.windows(2).position(|window| window == b"\r\n")?;
    let mut line: Vec<u8> = buf.drain(..=position + 1).collect();
    line.truncate(line.len() - 2);
    Some(line)
}

fn parse_request_line(line: &[u8]) -> Option<(String, String)> {
    let text = std::str::from_utf8(line).ok()?;
    let mut parts = text.split_ascii_whitespace();
    let method = parts.next()?;
    if !METHODS.contains(&method) {
        return None;
    }
    let path = parts.next()?;
    let version = parts.next()?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    Some((method.to_owned(), path.to_owned()))
}

fn parse_status_line(line: &[u8]) -> Option<u16> {
    let text = std::str::from_utf8(line).ok()?;
    let mut parts = text.split_ascii_whitespace();
    let version = parts.next()?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    let code: u16 = parts.next()?.parse().ok()?;
    (100..=599).contains(&code).then_some(code)
}

/// Match a `Content-Length: N` header line (case-insensitive name, per
/// HTTP) and return `N`. Any other header is `None` and left to the caller
/// to ignore — this is the one header we need to know where a
/// `Content-Length`-framed body ends.
fn parse_content_length(line: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(line).ok()?;
    let (name, value) = text.split_once(':')?;
    if !name.trim().eq_ignore_ascii_case("content-length") {
        return None;
    }
    value.trim().parse().ok()
}

/// Format a byte count for humans (`512B`, `1.2KB`, `3.4MB`, ...).
pub(crate) fn human_bytes(bytes: u64) -> String {
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

/// Format a duration for humans (`45ms`, `3.2s`).
pub(crate) fn human_duration(duration: Duration) -> String {
    let millis = duration.as_millis();
    if millis < 1000 {
        format!("{millis}ms")
    } else {
        format!("{:.1}s", duration.as_secs_f64())
    }
}

/// Which side of the proxied connection a [`Tee`] is observing.
#[derive(Clone, Copy)]
pub(crate) enum Direction {
    /// Local client → producer (carries the HTTP request, if any).
    Upstream,
    /// Producer → local client (carries the HTTP response, if any).
    Downstream,
}

/// An [`AsyncRead`] wrapper that forwards reads to `inner` unchanged and
/// feeds a copy of every byte read to a shared [`AccessLog`] — observation
/// only, never buffering, delaying, or altering the data path. Completed
/// downstream log lines are printed (narrate-gated) as soon as they're
/// detected.
pub(crate) struct Tee<R> {
    inner: R,
    log: Arc<StdMutex<AccessLog>>,
    direction: Direction,
    /// How this connection is identified in printed lines — `port connect`
    /// shows the local client's real `SocketAddr`; `port listen` has no
    /// local IP:port for its inbound QUIC peers to show, so its lines carry
    /// no label at all.
    label: Option<String>,
    narrate: bool,
}

impl<R> Tee<R> {
    pub(crate) fn new(
        inner: R,
        log: Arc<StdMutex<AccessLog>>,
        direction: Direction,
        label: Option<String>,
        narrate: bool,
    ) -> Self {
        Self {
            inner,
            log,
            direction,
            label,
            narrate,
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for Tee<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let me = self.get_mut();
        let before = buf.filled().len();
        let poll = Pin::new(&mut me.inner).poll_read(context, buf);
        if poll.is_ready() {
            let read = &buf.filled()[before..];
            if !read.is_empty() {
                // Held only across this synchronous block, never across an
                // `.await` — `await_holding_lock` is `deny`d workspace-wide.
                let mut log = me.log.lock().unwrap();
                match me.direction {
                    Direction::Upstream => log.feed_upstream(read),
                    Direction::Downstream => {
                        for line in log.feed_downstream(read) {
                            print_log_line(me.label.as_deref(), &line, me.narrate);
                        }
                    }
                }
            }
        }
        poll
    }
}

pub(crate) fn print_log_line(label: Option<&str>, line: &LogLine, narrate: bool) {
    let prefix = label.map_or_else(String::new, |label| format!("{label} "));
    let message = format!(
        "{prefix}{} {} → {} ({}, {})",
        line.method,
        line.path,
        line.status,
        human_bytes(line.bytes),
        human_duration(line.latency)
    );
    tracing::info!("{message}");
    if narrate {
        println!("{message}");
    }
}

#[cfg(test)]
mod tests {
    use super::{AccessLog, Direction, Tee, human_bytes, human_duration};
    use rand::{RngCore, SeedableRng, rngs::StdRng};
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn parses_a_simple_request_response_pair() {
        let mut log = AccessLog::new();
        log.feed_upstream(b"GET /api/foo HTTP/1.1\r\nHost: x\r\n\r\n");
        let response: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
        let completed = log.feed_downstream(response);
        assert!(
            completed.is_empty(),
            "first response stays open until the next boundary or finish()"
        );
        let line = log.finish().expect("response was open");
        assert_eq!(line.method, "GET");
        assert_eq!(line.path, "/api/foo");
        assert_eq!(line.status, 200);
        assert_eq!(
            line.bytes,
            response.len() as u64,
            "the whole message (status + headers + body) counts toward size"
        );
    }

    #[test]
    fn pairs_two_keep_alive_requests_in_order() {
        // A real `Content-Length`-framed body has no trailing `\r\n` of its
        // own — the next response's status line follows immediately. This
        // is exactly the case `Content-Length` skipping exists to handle:
        // without it, "body1" would glue onto the next status line and
        // break detection for the rest of the connection.
        let mut log = AccessLog::new();
        log.feed_upstream(b"GET /a HTTP/1.1\r\n\r\n");
        log.feed_upstream(b"GET /b HTTP/1.1\r\n\r\n");
        let mut completed =
            log.feed_downstream(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nbody1");
        completed.extend(
            log.feed_downstream(b"HTTP/1.1 404 Not Found\r\nContent-Length: 5\r\n\r\nbody2"),
        );
        assert_eq!(completed.len(), 1, "only the first response completes here");
        assert_eq!(completed[0].path, "/a");
        assert_eq!(completed[0].status, 200);
        assert_eq!(
            completed[0].bytes,
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nbody1".len() as u64
        );

        let last = log.finish().expect("second response was open");
        assert_eq!(last.path, "/b");
        assert_eq!(last.status, 404);
        assert_eq!(
            last.bytes,
            "HTTP/1.1 404 Not Found\r\nContent-Length: 5\r\n\r\nbody2".len() as u64
        );
    }

    #[test]
    fn content_length_body_split_across_chunks_does_not_corrupt_the_next_response() {
        // Same scenario as `pairs_two_keep_alive_requests_in_order`, but the
        // body arrives in a separate read from its headers, exercising the
        // `SkippingBody` byte-budget carrying over between calls.
        let mut log = AccessLog::new();
        log.feed_upstream(b"GET /a HTTP/1.1\r\n\r\n");
        log.feed_upstream(b"GET /b HTTP/1.1\r\n\r\n");
        let mut completed = log.feed_downstream(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nbo");
        completed
            .extend(log.feed_downstream(b"dy1HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"));
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].path, "/a");
        assert_eq!(completed[0].status, 200);

        let last = log.finish().expect("second response was open");
        assert_eq!(last.path, "/b");
        assert_eq!(last.status, 404);
    }

    #[test]
    fn request_line_split_across_chunks_still_parses() {
        let mut log = AccessLog::new();
        log.feed_upstream(b"GET /sp");
        log.feed_upstream(b"lit HTTP/1.1\r\n\r\n");
        log.feed_downstream(b"HTTP/1.1 204 No Content\r\n\r\n");
        let line = log.finish().expect("response was open");
        assert_eq!(line.path, "/split");
    }

    #[test]
    fn non_http_first_line_disables_sniffing_permanently() {
        let mut log = AccessLog::new();
        log.feed_upstream(b"\x00\x01\x02\x03 not http at all\r\n");
        // Even a well-formed-looking request line after the fact must not
        // re-enable sniffing for this connection.
        log.feed_upstream(b"GET /late HTTP/1.1\r\n\r\n");
        let completed = log.feed_downstream(b"HTTP/1.1 200 OK\r\n\r\n");
        assert!(completed.is_empty());
        assert!(log.finish().is_none());
    }

    #[test]
    fn oversized_first_line_without_crlf_disables_sniffing() {
        let mut log = AccessLog::new();
        log.feed_upstream(&vec![b'a'; super::MAX_LINE_LEN + 1]);
        log.feed_upstream(b"GET /late HTTP/1.1\r\n\r\n");
        let completed = log.feed_downstream(b"HTTP/1.1 200 OK\r\n\r\n");
        assert!(completed.is_empty());
        assert!(log.finish().is_none());
    }

    #[test]
    fn orphan_status_line_without_a_pending_request_is_ignored() {
        let mut log = AccessLog::new();
        log.feed_upstream(b"GET /a HTTP/1.1\r\n\r\n");
        // A second status line arrives with no second request queued — it
        // must be ignored rather than corrupting or replacing the response
        // that's legitimately paired and still open.
        let completed =
            log.feed_downstream(b"HTTP/1.1 200 OK\r\n\r\nHTTP/1.1 404 Not Found\r\n\r\n");
        assert!(
            completed.is_empty(),
            "the orphan line must not force-close the open response"
        );
        let line = log
            .finish()
            .expect("the legitimately paired response is still open");
        assert_eq!(line.path, "/a");
        assert_eq!(line.status, 200);
    }

    #[test]
    fn head_response_with_content_length_never_swallows_the_next_response() {
        // A HEAD response commonly carries the Content-Length a GET would
        // have had, but per HTTP/1.1 it never actually sends a body.
        let mut log = AccessLog::new();
        log.feed_upstream(b"HEAD /big HTTP/1.1\r\n\r\n");
        log.feed_upstream(b"GET /next HTTP/1.1\r\n\r\n");
        let completed = log.feed_downstream(
            b"HTTP/1.1 200 OK\r\nContent-Length: 999999\r\n\r\n\
              HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello",
        );
        assert_eq!(
            completed.len(),
            1,
            "the HEAD response closes as soon as the next status line is found"
        );
        assert_eq!(completed[0].method, "HEAD");
        assert_eq!(completed[0].status, 200);

        let last = log.finish().expect("the GET response is still open");
        assert_eq!(last.method, "GET");
        assert_eq!(last.path, "/next");
        assert_eq!(last.status, 200);
    }

    #[test]
    fn conditional_304_response_with_content_length_never_swallows_the_next_response() {
        let mut log = AccessLog::new();
        log.feed_upstream(b"GET /cached HTTP/1.1\r\n\r\n");
        log.feed_upstream(b"GET /next HTTP/1.1\r\n\r\n");
        let completed = log.feed_downstream(
            b"HTTP/1.1 304 Not Modified\r\nContent-Length: 999999\r\n\r\n\
              HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello",
        );
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].path, "/cached");
        assert_eq!(completed[0].status, 304);

        let last = log.finish().expect("the second response is still open");
        assert_eq!(last.path, "/next");
        assert_eq!(last.status, 200);
    }

    #[test]
    fn human_bytes_formats_common_magnitudes() {
        assert_eq!(human_bytes(0), "0B");
        assert_eq!(human_bytes(512), "512B");
        assert_eq!(human_bytes(1536), "1.5KB");
        assert_eq!(human_bytes(3 * 1024 * 1024 + 400 * 1024), "3.4MB");
    }

    #[test]
    fn human_duration_switches_units_at_one_second() {
        assert_eq!(human_duration(Duration::from_millis(45)), "45ms");
        assert_eq!(human_duration(Duration::from_millis(3200)), "3.2s");
    }

    // ── Tee adapter: the core forwarding path must stay byte-identical ──

    /// Write `data` into a duplex pipe in chunks of `chunk_size`, read it
    /// back out through a `Tee`-wrapped reader via `tokio::io::copy`, and
    /// return what came out the other end. The sniffer attached to the tee
    /// observes every byte but must never alter what's forwarded — that's
    /// exactly what this harness checks, regardless of the chunking.
    async fn round_trip_through_tee(data: &[u8], chunk_size: usize) -> Vec<u8> {
        let (mut writer, reader) = tokio::io::duplex(64 * 1024);
        let log = Arc::new(StdMutex::new(AccessLog::new()));
        let mut tee = Tee::new(
            reader,
            log,
            Direction::Upstream,
            Some("127.0.0.1:0".to_owned()),
            false,
        );
        let payload = data.to_vec();
        let writer_task = tokio::spawn(async move {
            for chunk in payload.chunks(chunk_size.max(1)) {
                writer.write_all(chunk).await.expect("write chunk");
            }
            drop(writer);
        });
        let mut sink = Vec::new();
        tokio::io::copy(&mut tee, &mut sink)
            .await
            .expect("copy through tee");
        writer_task.await.expect("writer task");
        sink
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tee_forwards_arbitrary_bytes_unchanged_across_chunk_sizes() {
        let mut rng = StdRng::seed_from_u64(0xACED_5EED);
        let mut data = vec![0u8; 70_000];
        rng.fill_bytes(&mut data);
        // Sprinkle in CRLFs so the sniffer's line scanning is actually
        // exercised on this payload, not just skipped as obviously binary.
        for index in (0..data.len()).step_by(997) {
            data[index] = b'\r';
            if index + 1 < data.len() {
                data[index + 1] = b'\n';
            }
        }
        for chunk_size in [1, 7, 64, 4096, 70_000] {
            let out = round_trip_through_tee(&data, chunk_size).await;
            assert_eq!(
                out, data,
                "tee must forward bytes unchanged at chunk_size={chunk_size}"
            );
        }
    }
}
