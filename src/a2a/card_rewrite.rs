//! Rewrite the A2A Agent Card's absolute endpoint URLs to the local bridge
//! address as the response streams back to the client.
//!
//! Why this exists: an Agent Card's `url` is absolute. A raw tunnel would hand
//! a remote client `http://origin-host:9999/…`, which the client then tries to
//! reach on *its own* network — defeating the bridge. Rewriting `url` (and
//! `additionalInterfaces[].url`) to `http://127.0.0.1:LOCAL`, path preserved,
//! makes the client POST back through the bridge.
//!
//! Detection is by content, not by pairing requests to responses: only a
//! bounded `application/json` response is buffered and parsed; if it looks like
//! a card (a well-known card, or a JSON-RPC envelope whose `result` is one) its
//! URLs are rewritten, otherwise it is emitted untouched. Every other response
//! — `text/event-stream` (SSE), chunked, close-delimited, oversized — streams
//! through byte-for-byte, so `message/stream` and `tasks/*` are never buffered.

use serde_json::Value;

/// Cap on a buffered card response. A real Agent Card is a few KB; anything
/// larger is not a card and is streamed through unbuffered rather than held.
const MAX_CARD_BYTES: usize = 1024 * 1024;

/// Cap on the header block before we give up framing this connection and fall
/// back to raw passthrough — real HTTP headers are far smaller.
const MAX_HEAD_BYTES: usize = 64 * 1024;

/// Where the downstream scanner is within the current response.
enum State {
    /// Accumulating the status line + header block until the blank line.
    Head,
    /// A card candidate (`application/json`, `Content-Length` ≤ cap): collect
    /// the whole body, then rewrite and emit with a corrected length.
    BufferBody {
        header: Vec<u8>,
        body: Vec<u8>,
        remaining: usize,
    },
    /// A framed, non-card body: pass exactly `remaining` bytes through, then
    /// look for the next response.
    StreamBody { remaining: usize },
}

/// A streaming rewriter for one bi-stream's response direction. Fed response
/// bytes with [`Self::feed`]; returns the bytes to forward to the client.
pub(super) struct CardRewriter {
    /// The local bridge base to rewrite card URLs onto, e.g.
    /// `http://127.0.0.1:8080` — no trailing slash, no path.
    local_base: String,
    state: State,
    pending: Vec<u8>,
    /// Latched once we hit a response we cannot cleanly frame (chunked,
    /// close-delimited, over-long header): everything after streams verbatim.
    /// Safe because the card is fetched first, on its own framed response.
    raw: bool,
}

impl CardRewriter {
    pub(super) fn new(local_base: String) -> Self {
        Self {
            local_base,
            state: State::Head,
            pending: Vec::new(),
            raw: false,
        }
    }

    /// Feed a chunk of response bytes; returns the (possibly rewritten) bytes to
    /// write to the client.
    pub(super) fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(chunk);
        let mut out = Vec::new();
        self.process(&mut out);
        out
    }

    /// Flush any bytes still buffered when the response stream ends (a card
    /// whose body never completed is emitted verbatim rather than dropped).
    pub(super) fn finish(&mut self) -> Vec<u8> {
        let mut out = std::mem::take(&mut self.pending);
        if let State::BufferBody { header, body, .. } =
            std::mem::replace(&mut self.state, State::Head)
        {
            let mut flushed = header;
            flushed.extend_from_slice(&body);
            flushed.extend_from_slice(&out);
            out = flushed;
        }
        out
    }

    fn process(&mut self, out: &mut Vec<u8>) {
        loop {
            if self.raw {
                out.append(&mut self.pending);
                return;
            }
            match &mut self.state {
                State::Head => {
                    let Some(end) = find_header_end(&self.pending) else {
                        if self.pending.len() > MAX_HEAD_BYTES {
                            self.raw = true;
                        }
                        return;
                    };
                    let header: Vec<u8> = self.pending.drain(..end).collect();
                    self.dispatch_header(header, out);
                }
                State::StreamBody { remaining } => {
                    let take = (*remaining).min(self.pending.len());
                    out.extend(self.pending.drain(..take));
                    *remaining -= take;
                    if *remaining > 0 {
                        return;
                    }
                    self.state = State::Head;
                }
                State::BufferBody {
                    body, remaining, ..
                } => {
                    let take = (*remaining).min(self.pending.len());
                    body.extend(self.pending.drain(..take));
                    *remaining -= take;
                    if *remaining > 0 {
                        return;
                    }
                    self.emit_buffered(out);
                }
            }
        }
    }

    /// Classify a just-completed header block and set the next state.
    fn dispatch_header(&mut self, header: Vec<u8>, out: &mut Vec<u8>) {
        let info = HeaderInfo::parse(&header);
        if info.chunked || (info.content_length.is_none() && info.has_body()) {
            // Un-frameable for keep-alive purposes: emit the header and stream
            // the rest of the connection verbatim.
            out.extend_from_slice(&header);
            self.raw = true;
            return;
        }
        let Some(len) = info.content_length else {
            // No body (204/304/1xx, or Content-Length: 0 implied) — header only.
            out.extend_from_slice(&header);
            self.state = State::Head;
            return;
        };
        if info.json && len <= MAX_CARD_BYTES {
            self.state = State::BufferBody {
                header,
                body: Vec::with_capacity(len),
                remaining: len,
            };
        } else {
            out.extend_from_slice(&header);
            self.state = State::StreamBody { remaining: len };
        }
    }

    /// A buffered card candidate is complete: rewrite if it is a card, else
    /// emit it verbatim.
    fn emit_buffered(&mut self, out: &mut Vec<u8>) {
        let State::BufferBody { header, body, .. } =
            std::mem::replace(&mut self.state, State::Head)
        else {
            return;
        };
        if let Some(rewritten) = rewrite_card_body(&body, &self.local_base) {
            out.extend_from_slice(&with_content_length(&header, rewritten.len()));
            out.extend_from_slice(&rewritten);
        } else {
            out.extend_from_slice(&header);
            out.extend_from_slice(&body);
        }
    }
}

/// The one past-the-end index of the `\r\n\r\n` that terminates the header
/// block, if present.
fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|start| start + 4)
}

/// The header facts the framer needs.
struct HeaderInfo {
    content_length: Option<usize>,
    chunked: bool,
    json: bool,
    status: u16,
}

impl HeaderInfo {
    fn parse(header: &[u8]) -> Self {
        let text = String::from_utf8_lossy(header);
        let mut lines = text.split("\r\n");
        let status = lines.next().and_then(parse_status_code).unwrap_or_default();
        let mut content_length = None;
        let mut chunked = false;
        let mut json = false;
        for line in lines {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let name = name.trim();
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.parse().ok();
            } else if name.eq_ignore_ascii_case("transfer-encoding") {
                chunked = value.to_ascii_lowercase().contains("chunked");
            } else if name.eq_ignore_ascii_case("content-type") {
                json = value.to_ascii_lowercase().contains("application/json");
            }
        }
        Self {
            content_length,
            chunked,
            json,
            status,
        }
    }

    /// Whether a response with this status line is allowed to carry a body at
    /// all (1xx / 204 / 304 never do, per HTTP/1.1).
    fn has_body(&self) -> bool {
        !(self.status == 204 || self.status == 304 || (100..200).contains(&self.status))
    }
}

fn parse_status_code(line: &str) -> Option<u16> {
    let mut parts = line.split_ascii_whitespace();
    let version = parts.next()?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse().ok()
}

/// Rebuild `header` with its `Content-Length` set to `len` (a card's body
/// grows or shrinks when its URLs are rewritten). A header block always has a
/// `Content-Length` here — this branch only runs for a buffered, length-framed
/// response — so the value is replaced in place.
fn with_content_length(header: &[u8], len: usize) -> Vec<u8> {
    let text = String::from_utf8_lossy(header);
    let mut rebuilt = String::with_capacity(text.len());
    for (index, line) in text.split("\r\n").enumerate() {
        if index > 0 {
            rebuilt.push_str("\r\n");
        }
        if line
            .split_once(':')
            .is_some_and(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        {
            rebuilt.push_str("Content-Length: ");
            rebuilt.push_str(&len.to_string());
        } else {
            rebuilt.push_str(line);
        }
    }
    rebuilt.into_bytes()
}

/// Parse `body` as JSON and, if it is an Agent Card (or a JSON-RPC envelope
/// wrapping one), rewrite its endpoint URLs onto `local_base`. Returns the
/// re-serialized body only when something changed; `None` leaves the original
/// bytes untouched (not JSON, not a card, or nothing to rewrite).
fn rewrite_card_body(body: &[u8], local_base: &str) -> Option<Vec<u8>> {
    let mut value: Value = serde_json::from_slice(body).ok()?;
    if rewrite_value(&mut value, local_base) {
        serde_json::to_vec(&value).ok()
    } else {
        None
    }
}

fn rewrite_value(value: &mut Value, local_base: &str) -> bool {
    let Some(map) = value.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    // JSON-RPC envelope: the authenticated extended card rides in `result`.
    if let Some(result) = map.get_mut("result") {
        changed |= rewrite_value(result, local_base);
    }
    if is_card(map) {
        changed |= rewrite_card_object(map, local_base);
    }
    changed
}

/// A card is a JSON object with a string `url` plus at least one field only an
/// Agent Card carries — so an unrelated object that merely has a `url` is left
/// alone.
fn is_card(map: &serde_json::Map<String, Value>) -> bool {
    map.get("url").is_some_and(Value::is_string)
        && (map.contains_key("skills")
            || map.contains_key("capabilities")
            || map.contains_key("protocolVersion"))
}

fn rewrite_card_object(map: &mut serde_json::Map<String, Value>, local_base: &str) -> bool {
    let mut changed = false;
    if let Some(Value::String(url)) = map.get("url")
        && let Some(rewritten) = rewrite_url(url, local_base)
    {
        map.insert("url".to_owned(), Value::String(rewritten));
        changed = true;
    }
    if let Some(Value::Array(interfaces)) = map.get_mut("additionalInterfaces") {
        for interface in interfaces.iter_mut() {
            let Some(entry) = interface.as_object_mut() else {
                continue;
            };
            if let Some(Value::String(url)) = entry.get("url")
                && let Some(rewritten) = rewrite_url(url, local_base)
            {
                entry.insert("url".to_owned(), Value::String(rewritten));
                changed = true;
            }
        }
    }
    changed
}

/// Swap an absolute `http(s)://authority` for `local_base`, preserving the
/// path + query. A non-http(s) URL (a gRPC target, say) is left as-is (`None`).
fn rewrite_url(url: &str, local_base: &str) -> Option<String> {
    let after_scheme = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let rest_start = after_scheme
        .find(['/', '?', '#'])
        .map_or(after_scheme.len(), |index| index);
    let rest = &after_scheme[rest_start..];
    Some(format!("{local_base}{rest}"))
}

#[cfg(test)]
mod tests {
    use super::{CardRewriter, rewrite_url};

    const BASE: &str = "http://127.0.0.1:8080";

    /// Drive the whole `body` through in one feed plus a finish, returning the
    /// forwarded bytes.
    fn run(response: &[u8]) -> Vec<u8> {
        let mut rewriter = CardRewriter::new(BASE.to_owned());
        let mut out = rewriter.feed(response);
        out.extend(rewriter.finish());
        out
    }

    /// Same, but fed one byte at a time — the framing must not depend on how
    /// the stream is chunked.
    fn run_bytewise(response: &[u8]) -> Vec<u8> {
        let mut rewriter = CardRewriter::new(BASE.to_owned());
        let mut out = Vec::new();
        for byte in response {
            out.extend(rewriter.feed(&[*byte]));
        }
        out.extend(rewriter.finish());
        out
    }

    fn card_response(url: &str) -> Vec<u8> {
        let body = format!(r#"{{"protocolVersion":"0.3.0","name":"x","url":"{url}","skills":[]}}"#);
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        )
        .into_bytes()
    }

    fn body_of(response: &[u8]) -> String {
        let text = String::from_utf8_lossy(response);
        let (_, body) = text.split_once("\r\n\r\n").expect("has a body");
        body.to_owned()
    }

    #[test]
    fn rewrites_the_card_url_preserving_path() {
        let out = run(&card_response("http://origin.internal:9999/a2a"));
        let body = body_of(&out);
        assert!(
            body.contains(r#""url":"http://127.0.0.1:8080/a2a""#),
            "url rewritten to the local base, path preserved: {body}"
        );
    }

    #[test]
    fn corrects_the_content_length_after_rewrite() {
        let out = run(&card_response(
            "http://a-very-long-origin-hostname.example:9999/a2a",
        ));
        let text = String::from_utf8_lossy(&out);
        let declared: usize = text
            .split("\r\n")
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .and_then(|value| value.parse().ok())
            .expect("a Content-Length header");
        assert_eq!(
            declared,
            body_of(&out).len(),
            "length matches the rewritten body"
        );
    }

    #[test]
    fn rewrites_the_url_when_streamed_one_byte_at_a_time() {
        let out = run_bytewise(&card_response("http://origin.internal:9999/rpc"));
        assert!(body_of(&out).contains("http://127.0.0.1:8080/rpc"));
    }

    #[test]
    fn passes_a_non_card_json_response_through_unchanged() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"taskId":"abc","status":"working"}}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        )
        .into_bytes();
        assert_eq!(run(&response), response, "non-card JSON is byte-identical");
    }

    #[test]
    fn rewrites_the_extended_card_inside_a_json_rpc_envelope() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"0.3.0","name":"x","url":"http://origin:9999/rpc","capabilities":{}}}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        )
        .into_bytes();
        let out = run(&response);
        assert!(body_of(&out).contains("http://127.0.0.1:8080/rpc"));
    }

    #[test]
    fn streams_an_event_stream_through_untouched() {
        // SSE: text/event-stream, never buffered even though it has a card-ish
        // path — its Content-Type gates it out of the JSON-buffer branch.
        let response =
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 20\r\n\r\ndata: hello\r\n\r\nxxx";
        assert_eq!(run(response), response, "SSE bytes forwarded verbatim");
    }

    #[test]
    fn streams_a_chunked_response_through_untouched() {
        let response =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
        assert_eq!(run(response), response, "chunked bytes forwarded verbatim");
    }

    #[test]
    fn pairs_two_keep_alive_responses() {
        // A card, then a plain 204 — the second must not be swallowed by the
        // first's body framing.
        let mut first = card_response("http://origin:9999/a2a");
        first.extend_from_slice(b"HTTP/1.1 204 No Content\r\n\r\n");
        let out = run(&first);
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("http://127.0.0.1:8080/a2a"), "card rewritten");
        assert!(text.contains("204 No Content"), "trailing 204 survives");
    }

    #[test]
    fn rewrite_url_leaves_non_http_targets_alone() {
        assert_eq!(rewrite_url("grpc://origin:9999", BASE), None);
        assert_eq!(
            rewrite_url("http://origin:9999", BASE).as_deref(),
            Some("http://127.0.0.1:8080")
        );
        assert_eq!(
            rewrite_url("https://origin:9999/path?q=1", BASE).as_deref(),
            Some("http://127.0.0.1:8080/path?q=1")
        );
    }
}
