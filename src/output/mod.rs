use std::io::{IsTerminal, Write};
use std::sync::LazyLock;

use serde::Serialize;
use tokio::sync::mpsc::UnboundedSender;

use crate::protocol::{Message, MessageKind, PresenceSubtype};

// ── wire-shape types ─────────────────────────────────────────────
//
// The serde shapes for every JSON line the daemon emits on stdout
// (consumed by the `/swarm` skill and any `--output json` client).
// Field order/naming is part of the wire format — documented in
// AGENTS.md, pinned by the insta snapshots below. Module-private:
// nothing outside `output` needs them (`event_json` returns the
// rendered `String`, never these types).

/// One-shot events (everything except the `"event":"message"` family).
/// `#[serde(tag = "event")]` inlines the discriminator as the first field.
#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum SimpleEvent<'a> {
    Ready {
        swarm: &'a str,
        name: &'a str,
        nickname: &'a str,
    },
    MsgPosted {
        id: &'a str,
    },
    PeerTimeout {
        nickname: &'a str,
        last_seen_secs_ago: u64,
    },
    PeerReturn {
        nickname: &'a str,
    },
    Info {
        message: &'a str,
    },
    Error {
        message: &'a str,
    },
}

/// Common prefix for every `{"event":"message",...}` line. Field
/// order is part of the wire format (see AGENTS.md).
#[derive(Serialize)]
struct MessageHeader<'a> {
    pub event: &'static str,
    pub id: &'a str,
    #[serde(rename = "type")]
    pub ty: &'static str,
    pub swarm: &'a str,
    pub author: &'a str,
    pub ts: i64,
}

#[derive(Serialize)]
struct MsgLine<'a> {
    #[serde(flatten)]
    pub header: MessageHeader<'a>,
    pub body: &'a str,
    pub reply: Option<&'a str>,
    #[serde(rename = "self")]
    pub is_self: bool,
}

#[derive(Serialize)]
struct PresenceLine<'a> {
    #[serde(flatten)]
    pub header: MessageHeader<'a>,
    pub subtype: PresenceSubtype,
}

fn message_header<'a>(msg: &'a Message, ty: &'static str) -> MessageHeader<'a> {
    MessageHeader {
        event: "message",
        id: msg.id.as_str(),
        ty,
        swarm: msg.swarm.as_str(),
        author: msg.author.as_str(),
        ts: msg.timestamp,
    }
}

/// Output mode — chosen per event loop at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputMode {
    Human,
    Json,
    /// Silent mode — suppresses all stdout and stderr user-facing
    /// output. Used by the MCP server, where stdout is reserved for
    /// the JSON-RPC protocol transport and any print would corrupt
    /// the stream.
    Silent,
}

/// Owned, structured form of every line the daemon would print.
/// In-process consumers ([`crate::embed::SwarmSession::events`],
/// tests) receive these over a channel instead of scraping stdout;
/// the `Stream` sink renders the *same* data through `events.rs`, so
/// the wire format stays byte-identical.
///
/// `#[non_exhaustive]`: a future event kind must not be a breaking
/// change for embedders matching on this.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum OutputEvent {
    Ready {
        swarm: String,
        name: String,
        nickname: String,
    },
    SwarmId {
        id: String,
    },
    Message {
        msg: Box<Message>,
        is_self: bool,
    },
    Presence {
        msg: Box<Message>,
    },
    PeerTimeout {
        nickname: String,
        last_seen_secs_ago: u64,
    },
    PeerReturn {
        nickname: String,
    },
    MsgPosted {
        id: String,
    },
    Info {
        message: String,
    },
    Error {
        message: String,
    },
}

/// ANSI styling for the Human-mode `<nick>` / `#swarm` tokens.
/// Hand-rolled (no color dependency); emission is gated on a TTY +
/// `NO_COLOR` per stream (see [`stdout_color`] / [`stderr_color`]).
mod style {
    pub(super) const RESET: &str = "\x1b[0m";
    /// Bold green — the local member's own nickname.
    pub(super) const SELF_NICK: &str = "\x1b[1;32m";
    /// Bold cyan — a peer's nickname.
    pub(super) const PEER_NICK: &str = "\x1b[1;36m";
    /// Bold yellow — a swarm name.
    pub(super) const SWARM: &str = "\x1b[1;33m";
}

/// Color is on when the stream is a terminal and `NO_COLOR` is unset.
fn color_enabled(stream_is_terminal: bool) -> bool {
    stream_is_terminal && std::env::var_os("NO_COLOR").is_none()
}

/// Whether stdout should carry ANSI color. Cached — neither the TTY
/// status nor the env var changes at runtime, and piped/non-TTY output
/// (tests, the `/swarm` skill) auto-disables.
fn stdout_color() -> bool {
    static ENABLED: LazyLock<bool> = LazyLock::new(|| color_enabled(std::io::stdout().is_terminal()));
    *ENABLED
}

/// Stderr counterpart to [`stdout_color`] (presence/info/timeout lines).
fn stderr_color() -> bool {
    static ENABLED: LazyLock<bool> = LazyLock::new(|| color_enabled(std::io::stderr().is_terminal()));
    *ENABLED
}

/// The ANSI `(prefix, suffix)` wrapping a `#swarm` token — empty when
/// disabled, so callers interpolate inline with no allocation either way.
fn swarm_ansi(enabled: bool) -> (&'static str, &'static str) {
    if enabled {
        (style::SWARM, style::RESET)
    } else {
        ("", "")
    }
}

/// Per-event-loop output destination. Replaces the former
/// process-global `MODE`/`FILTER_SELF` statics so multiple in-process
/// sessions (embed facade, tests) can run with independent output
/// instead of racing a first-write-wins `OnceLock`. Threaded by
/// `&Output` (not `Copy` — `Capture` holds a channel sender).
#[derive(Debug, Clone)]
pub(crate) enum Output {
    /// Write to the process stdout/stderr in the given mode (CLI /
    /// MCP-silent). `self_nick` is the local member's nickname, used to
    /// paint your own `<nick>` distinctly from peers' in Human mode.
    Stream {
        mode: OutputMode,
        filter_self: bool,
        self_nick: Option<String>,
    },
    /// Push structured events into an in-process channel (embed
    /// facade / tests). Mode-agnostic: every event is captured.
    Capture {
        tx: UnboundedSender<OutputEvent>,
        filter_self: bool,
    },
}

impl Output {
    pub(crate) fn new(mode: OutputMode, filter_self: bool, self_nick: Option<String>) -> Self {
        Self::Stream {
            mode,
            filter_self,
            self_nick,
        }
    }

    /// Suppresses all stdout/stderr output (MCP server).
    pub(crate) fn silent() -> Self {
        Self::Stream {
            mode: OutputMode::Silent,
            filter_self: false,
            self_nick: None,
        }
    }

    /// The local member's nickname, when known (Human-mode self-color).
    fn self_nick(&self) -> Option<&str> {
        match self {
            Output::Stream { self_nick, .. } => self_nick.as_deref(),
            Output::Capture { .. } => None,
        }
    }

    /// The ANSI `(prefix, suffix)` wrapping a `<name>` token —
    /// [`style::SELF_NICK`] for the local member, else
    /// [`style::PEER_NICK`]; empty when disabled. Callers interpolate
    /// inline so a disabled line allocates nothing extra.
    fn nick_ansi(&self, name: &str, enabled: bool) -> (&'static str, &'static str) {
        if !enabled {
            return ("", "");
        }
        let color = if self.self_nick() == Some(name) {
            style::SELF_NICK
        } else {
            style::PEER_NICK
        };
        (color, style::RESET)
    }

    /// Colorize `<nick>` / `#swarm` tokens in a pre-composed `info`
    /// string. Safe because nicknames/swarm names exclude `< > #` and
    /// whitespace, so the markers are unambiguous delimiters and the
    /// string never carries free-form message-body text.
    fn highlight(&self, text: &str, enabled: bool) -> String {
        if !enabled {
            return text.to_owned();
        }
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '<' => {
                    let mut name = String::new();
                    let mut closed = false;
                    while let Some(&next) = chars.peek() {
                        chars.next();
                        if next == '>' {
                            closed = true;
                            break;
                        }
                        name.push(next);
                    }
                    if closed {
                        let (open, close) = self.nick_ansi(&name, true);
                        out.push_str(open);
                        out.push('<');
                        out.push_str(&name);
                        out.push('>');
                        out.push_str(close);
                    } else {
                        out.push('<');
                        out.push_str(&name);
                    }
                }
                '#' => {
                    let mut name = String::new();
                    while let Some(&next) = chars.peek() {
                        if next.is_whitespace() || matches!(next, '<' | '>' | '#') {
                            break;
                        }
                        name.push(next);
                        chars.next();
                    }
                    if name.is_empty() {
                        out.push('#');
                    } else {
                        let (open, close) = swarm_ansi(true);
                        out.push_str(open);
                        out.push('#');
                        out.push_str(&name);
                        out.push_str(close);
                    }
                }
                other => out.push(other),
            }
        }
        out
    }

    /// Capture every event into `tx` (embed facade / in-process
    /// tests).
    pub(crate) fn capture(tx: UnboundedSender<OutputEvent>) -> Self {
        Self::Capture {
            tx,
            filter_self: false,
        }
    }

    fn filters_self(&self) -> bool {
        match self {
            Self::Stream { filter_self, .. } | Self::Capture { filter_self, .. } => *filter_self,
        }
    }

    /// The one sink skeleton every event method shares: in `Capture`
    /// mode push the structured [`OutputEvent`]; in `Stream` mode hand
    /// the chosen [`OutputMode`] to `stream`, which renders that
    /// event's own human / JSON / (silent) form. Factoring this out is
    /// what removes the ~10× duplicated `match Stream/Capture`.
    fn dispatch(&self, event: impl FnOnce() -> OutputEvent, stream: impl FnOnce(OutputMode)) {
        match self {
            Output::Stream { mode, .. } => stream(*mode),
            Output::Capture { tx, .. } => {
                let _ = tx.send(event());
            }
        }
    }

    pub(crate) fn ready(&self, swarm: &str, name: &str, nickname: &str) {
        self.dispatch(
            || OutputEvent::Ready {
                swarm: swarm.to_owned(),
                name: name.to_owned(),
                nickname: nickname.to_owned(),
            },
            |mode| {
                // In human mode, `info("joined as <nick>")` covers this.
                if mode == OutputMode::Json {
                    emit_json(&SimpleEvent::Ready {
                        swarm,
                        name,
                        nickname,
                    });
                }
            },
        );
    }

    /// Print the bare `ahs…` swarm identifier (stderr) at startup.
    /// Human and JSON modes both emit it; Silent suppresses it. The
    /// integration harness greps this line for the swarm id.
    pub(crate) fn swarm_id_line(&self, id: &str) {
        self.dispatch(
            || OutputEvent::SwarmId { id: id.to_owned() },
            |mode| {
                if mode != OutputMode::Silent {
                    eprintln!("{id}");
                }
            },
        );
    }

    pub(crate) fn print_message(&self, msg: &Message) {
        self.print_message_ex(msg, false);
    }

    /// Print a message with an optional self flag (for echo-back).
    /// When `filter_self` is enabled, self-authored messages are
    /// suppressed. Presence events should use `print_presence` instead.
    pub(crate) fn print_message_ex(&self, msg: &Message, is_self: bool) {
        if matches!(msg.kind, MessageKind::PeerInfo | MessageKind::Digest) {
            return;
        }
        if is_self && self.filters_self() {
            return;
        }
        self.dispatch(
            || OutputEvent::Message {
                msg: Box::new(msg.clone()),
                is_self,
            },
            |mode| match mode {
                OutputMode::Human => self.print_message_human(msg),
                OutputMode::Json => print_message_json(msg, is_self),
                OutputMode::Silent => {}
            },
        );
    }

    pub(crate) fn print_presence(&self, msg: &Message) {
        let MessageKind::Presence { subtype } = &msg.kind else {
            return;
        };
        self.dispatch(
            || OutputEvent::Presence {
                msg: Box::new(msg.clone()),
            },
            |mode| match mode {
                OutputMode::Human => {
                    let (open, close) = self.nick_ansi(msg.author.as_str(), stderr_color());
                    eprintln!("{open}<{}>{close} has {subtype}", msg.author);
                }
                OutputMode::Json => emit(&format_presence_json(msg, *subtype)),
                OutputMode::Silent => {}
            },
        );
    }

    /// Emit a peer-timeout event. Distinct from `presence left` (a
    /// voluntary departure): this fires when the local heartbeat
    /// sweeper evicts a participant silent past `ALIVE_TIMEOUT_SECS`.
    pub(crate) fn peer_timeout(&self, nickname: &str, last_seen_secs_ago: u64) {
        self.dispatch(
            || OutputEvent::PeerTimeout {
                nickname: nickname.to_owned(),
                last_seen_secs_ago,
            },
            |mode| match mode {
                OutputMode::Human => {
                    let (open, close) = self.nick_ansi(nickname, stderr_color());
                    eprintln!("{open}<{nickname}>{close} went quiet");
                }
                OutputMode::Json => emit_json(&SimpleEvent::PeerTimeout {
                    nickname,
                    last_seen_secs_ago,
                }),
                OutputMode::Silent => {}
            },
        );
    }

    /// Symmetric counterpart to `peer_timeout`: fires when a peer we
    /// previously evicted for silence resumes sending (any message,
    /// including an `Alive` keepalive).
    pub(crate) fn peer_return(&self, nickname: &str) {
        self.dispatch(
            || OutputEvent::PeerReturn {
                nickname: nickname.to_owned(),
            },
            |mode| match mode {
                OutputMode::Human => {
                    let (open, close) = self.nick_ansi(nickname, stderr_color());
                    eprintln!("{open}<{nickname}>{close} came back");
                }
                OutputMode::Json => emit_json(&SimpleEvent::PeerReturn { nickname }),
                OutputMode::Silent => {}
            },
        );
    }

    /// Print a "message posted" confirmation with the message ID.
    pub(crate) fn msg_posted(&self, id: &str) {
        self.dispatch(
            || OutputEvent::MsgPosted { id: id.to_owned() },
            |mode| match mode {
                OutputMode::Human => {
                    eprintln!("message posted");
                    println!("{id}");
                }
                OutputMode::Json => emit_json(&SimpleEvent::MsgPosted { id }),
                OutputMode::Silent => {}
            },
        );
    }

    /// Print an informational line.
    pub(crate) fn info(&self, msg: &str) {
        self.dispatch(
            || OutputEvent::Info {
                message: msg.to_owned(),
            },
            |mode| match mode {
                OutputMode::Human => eprintln!("{}", self.highlight(msg, stderr_color())),
                OutputMode::Json => emit_json(&SimpleEvent::Info { message: msg }),
                OutputMode::Silent => {}
            },
        );
    }

    /// Print an error line.
    pub(crate) fn error(&self, msg: &str) {
        self.dispatch(
            || OutputEvent::Error {
                message: msg.to_owned(),
            },
            |mode| match mode {
                OutputMode::Human => eprintln!("error: {msg}"),
                OutputMode::Json => emit_json(&SimpleEvent::Error { message: msg }),
                OutputMode::Silent => {}
            },
        );
    }

    /// Surface any displayable error (`anyhow::Error`, `BodyError`, …)
    /// through the same sink as [`Output::error`] — the single funnel
    /// for interactive (and future) error reporting, so the
    /// `error:`-prefixed Human line and the structured `Error` event
    /// stay in lockstep without each caller stringifying by hand.
    pub(crate) fn report_error(&self, error: &impl std::fmt::Display) {
        self.error(&error.to_string());
    }

    /// Render a message in Human mode: `<author>: body`, or
    /// `<author> → <target>: body` for a reply. The author/target
    /// `<nick>` tokens are colored (self vs peer); the **body** is
    /// printed raw — it may legitimately contain `<`, `>`, `#`, so it
    /// must never be run through the colorizer.
    fn print_message_human(&self, msg: &Message) {
        let enabled = stdout_color();
        let (open, close) = self.nick_ansi(msg.author.as_str(), enabled);
        match &msg.kind {
            MessageKind::Msg {
                reply: Some(target),
            } => {
                let (to_open, to_close) = self.nick_ansi(target.as_str(), enabled);
                println!(
                    "{open}<{}>{close} → {to_open}<{}>{to_close}: {}",
                    msg.author, target, msg.body
                );
            }
            _ => println!("{open}<{}>{close}: {}", msg.author, msg.body),
        }
    }

    /// Clear the last line typed by the user (move up + erase). Human
    /// mode only; a no-op for JSON/Silent/Capture (terminal control
    /// has no structured form).
    pub(crate) fn clear_input_line(&self) {
        if let Output::Stream {
            mode: OutputMode::Human,
            ..
        } = self
        {
            eprint!("\x1b[1A\x1b[2K");
        }
    }
}

/// Write a line to stdout and flush immediately.
/// Required for Monitor compatibility — piped stdout is fully buffered,
/// so without explicit flush, events stall in an 8KB buffer.
fn emit(line: &str) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{line}");
    let _ = lock.flush();
}

fn emit_json<T: Serialize>(value: &T) {
    if let Ok(json) = serde_json::to_string(value) {
        emit(&json);
    }
}


/// Format a presence message as JSON.
///
/// Serializes the struct directly because the documented wire format
/// pins the field order (`event`, `id`, `type`, `swarm`, `author`,
/// `ts`, …) and `Value::to_string` would sort keys alphabetically.
fn format_presence_json(msg: &Message, subtype: PresenceSubtype) -> String {
    serde_json::to_string(&PresenceLine {
        header: message_header(msg, "presence"),
        subtype,
    })
    .expect("presence event serialization should never fail")
}

/// Format a Msg as a JSON string. Presence uses
/// `format_presence_json`; `PeerInfo` is never printed.
pub(crate) fn format_msg_json(msg: &Message, is_self: bool) -> String {
    match &msg.kind {
        MessageKind::Msg { reply } => serde_json::to_string(&MsgLine {
            header: message_header(msg, "msg"),
            body: msg.body.as_str(),
            reply: reply
                .as_ref()
                .map(super::protocol::nickname::Nickname::as_str),
            is_self,
        })
        .expect("message event serialization should never fail"),
        MessageKind::Presence { .. } | MessageKind::PeerInfo | MessageKind::Digest => {
            unreachable!("format_msg_json only handles Msg")
        }
    }
}

fn print_message_json(msg: &Message, is_self: bool) {
    emit(&format_msg_json(msg, is_self));
}

/// Render a captured [`OutputEvent`] to the exact JSON line the
/// daemon writes in `--output json` mode. Reuses the same `events.rs`
/// serializers as the `Stream` sink, so in-process tests assert the
/// byte-identical wire format the `/swarm` skill + MCP clients parse.
/// `None` for events that produce no JSON line in JSON mode
/// (`SwarmId` is the bare stderr `ahs…` line, never JSON).
#[must_use]
pub fn event_json(event: &OutputEvent) -> Option<String> {
    let json = match event {
        OutputEvent::Ready {
            swarm,
            name,
            nickname,
        } => serde_json::to_string(&SimpleEvent::Ready {
            swarm,
            name,
            nickname,
        }),
        OutputEvent::Message { msg, is_self } => return Some(format_msg_json(msg, *is_self)),
        OutputEvent::Presence { msg } => {
            let MessageKind::Presence { subtype } = &msg.kind else {
                return None;
            };
            return Some(format_presence_json(msg, *subtype));
        }
        OutputEvent::PeerTimeout {
            nickname,
            last_seen_secs_ago,
        } => serde_json::to_string(&SimpleEvent::PeerTimeout {
            nickname,
            last_seen_secs_ago: *last_seen_secs_ago,
        }),
        OutputEvent::PeerReturn { nickname } => {
            serde_json::to_string(&SimpleEvent::PeerReturn { nickname })
        }
        OutputEvent::MsgPosted { id } => serde_json::to_string(&SimpleEvent::MsgPosted { id }),
        OutputEvent::Info { message } => serde_json::to_string(&SimpleEvent::Info { message }),
        OutputEvent::Error { message } => serde_json::to_string(&SimpleEvent::Error { message }),
        OutputEvent::SwarmId { .. } => return None,
    };
    json.ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Message, PresenceSubtype};

    fn parse(text: &str) -> serde_json::Value {
        serde_json::from_str(text).unwrap_or_else(|error| panic!("invalid JSON: {error}\n{text}"))
    }

    // ── Human-mode coloring (style::*) ─────────────────────────

    fn human(self_nick: &str) -> Output {
        Output::new(OutputMode::Human, false, Some(self_nick.to_owned()))
    }

    #[test]
    fn nick_ansi_self_vs_peer() {
        let out = human("alice");
        assert_eq!(out.nick_ansi("alice", true), (style::SELF_NICK, style::RESET));
        assert_eq!(out.nick_ansi("bob", true), (style::PEER_NICK, style::RESET));
    }

    #[test]
    fn nick_ansi_disabled_is_empty() {
        let out = human("alice");
        assert_eq!(out.nick_ansi("alice", false), ("", ""));
        assert_eq!(out.nick_ansi("bob", false), ("", ""));
    }

    #[test]
    fn highlight_colors_swarm_and_self_and_peer() {
        let out = human("alice");
        let colored = out.highlight("created #team and joined as <alice>", true);
        assert_eq!(
            colored,
            format!(
                "created {}#team{} and joined as {}<alice>{}",
                style::SWARM,
                style::RESET,
                style::SELF_NICK,
                style::RESET
            )
        );
        // A peer nick in an info line gets the peer color.
        let peer = out.highlight("joined as <bob>", true);
        assert!(peer.contains(&format!("{}<bob>{}", style::PEER_NICK, style::RESET)));
    }

    #[test]
    fn highlight_disabled_is_identity() {
        let out = human("alice");
        let text = "created #team and joined as <alice>";
        assert_eq!(out.highlight(text, false), text);
    }

    // ── format_msg_json: msg type ──────────────────────────────

    fn nick(name: &str) -> crate::protocol::Nickname {
        crate::protocol::Nickname::from(name)
    }

    fn body(text: &str) -> crate::protocol::MessageBody {
        crate::protocol::MessageBody::from(text)
    }

    fn sid() -> crate::protocol::SwarmId {
        crate::protocol::SwarmId::from("ahstest")
    }

    #[test]
    fn json_message_has_all_fields() {
        let msg = Message::new_message(&sid(), &nick("alice"), body("hello"));
        let json = format_msg_json(&msg, false);
        let parsed = parse(&json);
        assert_eq!(parsed["event"], "message");
        assert_eq!(parsed["type"], "msg");
        assert_eq!(parsed["swarm"], "ahstest");
        assert_eq!(parsed["author"], "alice");
        assert_eq!(parsed["body"], "hello");
        assert!(parsed["reply"].is_null());
        assert_eq!(parsed["self"], false);
        assert!(parsed["id"].is_string());
        assert!(parsed["ts"].is_number());
    }

    #[test]
    fn json_reply_has_target_nickname() {
        let reply = Message::new_reply(&sid(), &nick("bob"), nick("alice"), body("a"));
        let parsed = parse(&format_msg_json(&reply, false));
        assert_eq!(parsed["reply"].as_str().unwrap(), "alice");
    }

    #[test]
    fn json_self_flag_true() {
        let msg = Message::new_message(&sid(), &nick("alice"), body("hi"));
        let parsed = parse(&format_msg_json(&msg, true));
        assert_eq!(parsed["self"], true);
    }

    #[test]
    fn json_body_escapes_quotes() {
        let msg = Message::new_message(&sid(), &nick("alice"), body(r#"say "hello""#));
        let json = format_msg_json(&msg, false);
        let parsed = parse(&json);
        assert_eq!(parsed["body"], r#"say "hello""#);
    }

    #[test]
    fn json_body_escapes_backslashes() {
        let msg = Message::new_message(&sid(), &nick("alice"), body(r"path\to\file"));
        let json = format_msg_json(&msg, false);
        let parsed = parse(&json);
        assert_eq!(parsed["body"], r"path\to\file");
    }

    // ── format_msg_json: presence type ─────────────────────────

    #[test]
    fn json_presence_joined() {
        let msg = Message::new_joined(&sid(), &nick("alice"));
        let parsed = parse(&format_presence_json(&msg, PresenceSubtype::Joined));
        assert_eq!(parsed["event"], "message");
        assert_eq!(parsed["type"], "presence");
        assert_eq!(parsed["subtype"], "joined");
        assert_eq!(parsed["author"], "alice");
        assert_eq!(parsed["swarm"], "ahstest");
    }

    #[test]
    fn json_presence_left() {
        let msg = Message::new_left(&sid(), &nick("bob"));
        let parsed = parse(&format_presence_json(&msg, PresenceSubtype::Left));
        assert_eq!(parsed["subtype"], "left");
        assert_eq!(parsed["author"], "bob");
    }

    // ── peer_timeout / peer_return shape ───────────────────────

    #[test]
    fn peer_timeout_json_shape() {
        let json = serde_json::to_string(&SimpleEvent::PeerTimeout {
            nickname: "ball-blue",
            last_seen_secs_ago: 94,
        })
        .unwrap();
        let parsed = parse(&json);
        assert_eq!(parsed["event"], "peer_timeout");
        assert_eq!(parsed["nickname"], "ball-blue");
        assert_eq!(parsed["last_seen_secs_ago"], 94);
    }

    #[test]
    fn peer_return_json_shape() {
        let json = serde_json::to_string(&SimpleEvent::PeerReturn {
            nickname: "ball-blue",
        })
        .unwrap();
        let parsed = parse(&json);
        assert_eq!(parsed["event"], "peer_return");
        assert_eq!(parsed["nickname"], "ball-blue");
    }

    // ── all outputs are valid single-line JSON ─────────────────────

    #[test]
    fn json_output_is_single_line() {
        let msgs: Vec<String> = vec![
            format_msg_json(
                &Message::new_message(&sid(), &nick("alice"), body("hello world")),
                false,
            ),
            format_msg_json(
                &Message::new_reply(&sid(), &nick("bob"), nick("alice"), body("reply")),
                false,
            ),
            format_presence_json(
                &Message::new_joined(&sid(), &nick("charlie")),
                PresenceSubtype::Joined,
            ),
            format_presence_json(
                &Message::new_left(&sid(), &nick("dave")),
                PresenceSubtype::Left,
            ),
        ];
        for json in msgs {
            assert!(!json.contains('\n'), "JSON should be single line: {json}");
            assert!(parse(&json).is_object());
        }
    }

    mod snapshots {
        use super::*;
        use crate::protocol::{Message, MessageKind, PresenceSubtype};

        #[test]
        fn snap_msg_message() {
            let msg = Message::fixture(MessageKind::Msg { reply: None }, "What is Rust?");
            insta::assert_snapshot!(format_msg_json(&msg, false));
        }

        #[test]
        fn snap_msg_reply() {
            let msg = Message::fixture(
                MessageKind::Msg {
                    reply: Some(crate::protocol::Nickname::from("addressed-nick")),
                },
                "Rust is a systems language.",
            );
            insta::assert_snapshot!(format_msg_json(&msg, false));
        }

        #[test]
        fn snap_presence_joined() {
            let msg = Message::fixture(
                MessageKind::Presence {
                    subtype: PresenceSubtype::Joined,
                },
                "",
            );
            insta::assert_snapshot!(format_presence_json(&msg, PresenceSubtype::Joined));
        }

        #[test]
        fn snap_presence_left() {
            let msg = Message::fixture(
                MessageKind::Presence {
                    subtype: PresenceSubtype::Left,
                },
                "",
            );
            insta::assert_snapshot!(format_presence_json(&msg, PresenceSubtype::Left));
        }
    }

    mod prop {
        use super::*;
        use crate::protocol::Message;
        use proptest::collection::vec as arb_vec;
        use proptest::prelude::*;

        fn arb_ascii_body() -> impl Strategy<Value = String> {
            arb_vec(0x20u8..0x7Eu8, 0..200).prop_map(|bytes| String::from_utf8(bytes).unwrap())
        }

        fn arb_nickname() -> impl Strategy<Value = crate::protocol::Nickname> {
            "[a-z]{3,8}-[a-z]{3,8}".prop_map(|raw| crate::protocol::Nickname::new(raw).unwrap())
        }

        proptest! {
            #[test]
            fn prop_message_json_is_valid(
                body_text in arb_ascii_body(),
                author in arb_nickname(),
                is_self in any::<bool>(),
            ) {
                let expected = body_text.clone();
                let msg = Message::new_message(
                    &sid(),
                    &author,
                    crate::protocol::MessageBody::new(body_text).unwrap(),
                );
                let json = format_msg_json(&msg, is_self);
                let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
                prop_assert_eq!(parsed["body"].as_str().unwrap(), expected.as_str());
                prop_assert!(!json.contains('\n'));
            }

            #[test]
            fn prop_reply_json_is_valid(
                body_text in arb_ascii_body(),
                author in arb_nickname(),
                target in arb_nickname(),
            ) {
                let expected_target = target.as_str().to_string();
                let expected_body = body_text.clone();
                let msg = Message::new_reply(
                    &sid(),
                    &author,
                    target,
                    crate::protocol::MessageBody::new(body_text).unwrap(),
                );
                let json = format_msg_json(&msg, false);
                let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
                prop_assert_eq!(parsed["reply"].as_str().unwrap(), expected_target.as_str());
                prop_assert_eq!(parsed["body"].as_str().unwrap(), expected_body.as_str());
            }

            #[test]
            fn prop_presence_json_is_valid(is_join in any::<bool>()) {
                let test_nick = crate::protocol::Nickname::from("test-nick");
                let (msg, subtype) = if is_join {
                    (Message::new_joined(&sid(), &test_nick), PresenceSubtype::Joined)
                } else {
                    (Message::new_left(&sid(), &test_nick), PresenceSubtype::Left)
                };
                let json = format_presence_json(&msg, subtype);
                let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
                prop_assert_eq!(&parsed["type"], "presence");
            }
        }
    }
}
