//! The JSON wire layer: the serde shapes for every line the daemon
//! emits on stdout (consumed by the `/swarm` skill and any `--output
//! json` client) plus the serializers that render them. Field
//! order/naming is part of the wire format — documented in AGENTS.md,
//! pinned by the insta snapshots in `tests`. The `Output` sink in the
//! parent module renders through these so the captured-event and
//! stdout forms stay byte-identical.

use std::fmt::Write as _;
use std::io::Write;

use serde::Serialize;

use super::OutputEvent;
use crate::protocol::{
    ExchangeKind, ExchangePhase, Message, MessageKind, Nickname, PresenceSubtype,
};

/// One-shot events (everything except the `"event":"message"` family).
/// `#[serde(tag = "event")]` inlines the discriminator as the first field.
#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(super) enum SimpleEvent<'a> {
    Ready {
        version: &'a str,
        swarm: &'a str,
        name: &'a str,
        nickname: &'a str,
        /// Skill-drift warning for a stale install; omitted from the wire when
        /// the install is current, so the common case stays unchanged.
        #[serde(skip_serializing_if = "Option::is_none")]
        drift: Option<&'a str>,
    },
    MsgPosted {
        id: &'a str,
    },
    PeerTimeout {
        nickname: &'a str,
        last_seen_secs_ago: u64,
        /// Pre-formatted, markdown-safe line the `/swarm` skill echoes
        /// verbatim. See [`peer_timeout_display`].
        display: String,
    },
    PeerReturn {
        nickname: &'a str,
        /// Pre-formatted, markdown-safe line (see [`peer_return_display`]).
        display: String,
    },
    Fork {
        nickname: &'a str,
        pubkey: &'a str,
        seq: u64,
    },
    ExchangeTimeout {
        exchange_id: &'a str,
    },
    Info {
        message: &'a str,
    },
    Error {
        message: &'a str,
    },
    PingReport {
        peers: Vec<PingPeer>,
        responded: usize,
        known: usize,
        /// Pre-formatted, markdown-safe RTT table (see [`ping_report_display`]).
        display: String,
    },
}

/// One peer's RTT in a `ping_report` event.
#[derive(Debug, Clone, Serialize)]
pub struct PingPeer {
    pub nickname: Nickname,
    pub rtt_ms: u64,
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
    /// Author's full Ed25519 public key (hex) — the cryptographic identity
    /// behind the display `author`. `Some` on every signed (real) message;
    /// `None` (omitted) only for unsigned test fixtures. Agents should key
    /// trust/disambiguation on this, not the nickname.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pubkey: Option<&'a str>,
    pub ts: i64,
}

#[derive(Serialize)]
struct MsgLine<'a> {
    #[serde(flatten)]
    pub header: MessageHeader<'a>,
    pub body: &'a str,
    pub reply: Option<&'a str>,
    /// Pre-formatted, markdown-safe line the `/swarm` skill echoes
    /// verbatim — the single source of truth for what the user sees.
    /// See [`msg_display`].
    pub display: String,
    #[serde(rename = "self")]
    pub is_self: bool,
}

#[derive(Serialize)]
struct PresenceLine<'a> {
    #[serde(flatten)]
    pub header: MessageHeader<'a>,
    pub subtype: PresenceSubtype,
    /// Pre-formatted, markdown-safe line (see [`presence_display`]).
    pub display: String,
}

/// A `{"event":"exchange",...}` line for a **content** exchange leg. A distinct
/// top-level event (not the `message` family) so skills branch on
/// `event`; field order is part of the wire format.
#[derive(Serialize)]
struct ExchangeLine<'a> {
    pub event: &'static str,
    pub id: &'a str,
    pub swarm: &'a str,
    pub author: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pubkey: Option<&'a str>,
    pub ts: i64,
    pub to: &'a str,
    pub exchange_id: &'a str,
    pub kind: ExchangeKind,
    pub phase: ExchangePhase,
    pub body: &'a str,
    /// Pre-formatted, markdown-safe line (see [`exchange_display`]).
    pub display: String,
    #[serde(rename = "self")]
    pub is_self: bool,
}

/// A `{"event":"exchange_progress",...}` line for the `Progress` phase — the
/// receiver's liveness+percent heartbeat. `done`/`total` are `None` when
/// the receiver reports indeterminate progress (no fraction in the body).
#[derive(Serialize)]
struct ExchangeProgressLine<'a> {
    pub event: &'static str,
    pub id: &'a str,
    pub swarm: &'a str,
    pub author: &'a str,
    pub ts: i64,
    pub to: &'a str,
    pub exchange_id: &'a str,
    pub kind: ExchangeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    pub display: String,
    #[serde(rename = "self")]
    pub is_self: bool,
}

/// Parse a `Progress` body's `done/total` fraction (e.g. `"35/100"`).
/// Returns `(None, None)` for an empty or unparseable body — an
/// indeterminate "still working" beat.
fn parse_progress(body: &str) -> (Option<u64>, Option<u64>) {
    let Some((done, total)) = body.split_once('/') else {
        return (None, None);
    };
    match (done.trim().parse::<u64>(), total.trim().parse::<u64>()) {
        (Ok(done), Ok(total)) => (Some(done), Some(total)),
        _ => (None, None),
    }
}

fn message_header<'a>(msg: &'a Message, ty: &'static str) -> MessageHeader<'a> {
    MessageHeader {
        event: "message",
        id: msg.id.as_str(),
        ty,
        swarm: msg.swarm.as_str(),
        author: msg.author.as_str(),
        pubkey: (!msg.pubkey.is_empty()).then_some(msg.pubkey.as_str()),
        ts: msg.timestamp,
    }
}

/// The pre-formatted, markdown-safe `display` line for a `msg` event —
/// the single source of truth the `/swarm` skill echoes verbatim, so the
/// model never composes or re-types a body. Nicks are wrapped in literal
/// backticks: the skill renders into markdown, where a bare `<nick>` is
/// stripped as an HTML tag, and the code span prevents that. The body is
/// embedded **raw** (never trimmed, escaped, or re-spaced). This is
/// JSON-only — the Human/terminal sink renders separately (ANSI, no
/// backticks).
fn msg_display(author: &str, body: &str, reply: Option<&str>) -> String {
    match reply {
        Some(target) => format!("🐝️ `<{author}>` → `<{target}>`: {body}"),
        None => format!("🐝️ `<{author}>`: {body}"),
    }
}

/// `display` line for an `exchange` event:
/// `` 🐝️ exchange handover offer `<author>` → `<to>`: body ``. See
/// [`msg_display`] for the backtick rationale. The skill may render a
/// richer interaction (the tasks widget, collapsed status lines) instead
/// of echoing this verbatim; it is the canonical line for raw
/// `--output json` consumers.
fn exchange_display(
    author: &str,
    to: &str,
    kind: ExchangeKind,
    phase: ExchangePhase,
    body: &str,
) -> String {
    format!("🐝️ exchange {kind} {phase} `<{author}>` → `<{to}>`: {body}")
}

/// `display` line for a `exchange_progress` event:
/// `` 🐝️ exchange handover progress `<author>` → `<to>`: 35/100 `` (or
/// `working` when indeterminate).
fn exchange_progress_display(
    author: &str,
    to: &str,
    kind: ExchangeKind,
    done: Option<u64>,
    total: Option<u64>,
) -> String {
    match (done, total) {
        (Some(done), Some(total)) => {
            format!("🐝️ exchange {kind} progress `<{author}>` → `<{to}>`: {done}/{total}")
        }
        _ => format!("🐝️ exchange {kind} progress `<{author}>` → `<{to}>`: working"),
    }
}

/// `display` line for a presence event: `` 🐝️ `<author>` has joined `` /
/// `… has left`. See [`msg_display`] for the backtick rationale.
fn presence_display(author: &str, subtype: PresenceSubtype) -> String {
    format!("🐝️ `<{author}>` has {subtype}")
}

/// `display` line for a `peer_timeout` event.
pub(super) fn peer_timeout_display(nickname: &str) -> String {
    format!("🐝️ `<{nickname}>` went quiet")
}

/// `display` line for a `peer_return` event.
pub(super) fn peer_return_display(nickname: &str) -> String {
    format!("🐝️ `<{nickname}>` came back")
}

/// `display` block for a `ping_report` event: a markdown RTT table (one
/// row per responding peer), or a single line when no peer answered.
pub(super) fn ping_report_display(peers: &[PingPeer], known: usize) -> String {
    if peers.is_empty() {
        return "🐝️ ping: no peers responded".to_owned();
    }
    let mut out = String::from("🐝️ ping\n| peer | RTT |\n|---|---|\n");
    for peer in peers {
        let _ = writeln!(out, "| `<{}>` | {}ms |", peer.nickname, peer.rtt_ms);
    }
    let _ = write!(out, "{}/{known} online", peers.len());
    out
}

/// Write a line to stdout and flush immediately.
/// Required for Monitor compatibility — piped stdout is fully buffered,
/// so without explicit flush, events stall in an 8KB buffer.
pub(super) fn emit(line: &str) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{line}");
    let _ = lock.flush();
}

pub(super) fn emit_json<T: Serialize>(value: &T) {
    if let Ok(json) = serde_json::to_string(value) {
        emit(&json);
    }
}

/// Format a presence message as JSON.
///
/// Serializes the struct directly because the documented wire format
/// pins the field order (`event`, `id`, `type`, `swarm`, `author`,
/// `ts`, …) and `Value::to_string` would sort keys alphabetically.
pub(super) fn format_presence_json(msg: &Message, subtype: PresenceSubtype) -> String {
    serde_json::to_string(&PresenceLine {
        header: message_header(msg, "presence"),
        subtype,
        display: presence_display(msg.author.as_str(), subtype),
    })
    .expect("presence event serialization should never fail")
}

/// Format a Msg as a JSON string. Presence uses
/// `format_presence_json`; `PeerInfo` is never printed.
pub(super) fn format_msg_json(msg: &Message, is_self: bool) -> String {
    match &msg.kind {
        MessageKind::Msg { reply } => serde_json::to_string(&MsgLine {
            header: message_header(msg, "msg"),
            body: msg.body.as_str(),
            reply: reply.as_ref().map(Nickname::as_str),
            display: msg_display(
                msg.author.as_str(),
                msg.body.as_str(),
                reply.as_ref().map(Nickname::as_str),
            ),
            is_self,
        })
        .expect("message event serialization should never fail"),
        MessageKind::Presence { .. }
        | MessageKind::PeerInfo
        | MessageKind::Digest
        | MessageKind::Ping
        | MessageKind::Pong { .. }
        | MessageKind::Exchange { .. } => {
            unreachable!("format_msg_json only handles Msg")
        }
    }
}

pub(super) fn print_message_json(msg: &Message, is_self: bool) {
    emit(&format_msg_json(msg, is_self));
}

/// Format an exchange leg as its JSON line. Only `Exchange` kinds reach here
/// (callers match first). The `Progress` phase renders as a
/// `exchange_progress` event; every other (content) phase as an `exchange` event.
pub(super) fn format_exchange_json(msg: &Message, is_self: bool) -> String {
    let MessageKind::Exchange {
        to,
        exchange_id,
        kind,
        phase,
    } = &msg.kind
    else {
        unreachable!("format_exchange_json only handles Exchange")
    };
    if matches!(phase, ExchangePhase::Progress) {
        let (done, total) = parse_progress(msg.body.as_str());
        return serde_json::to_string(&ExchangeProgressLine {
            event: "exchange_progress",
            id: msg.id.as_str(),
            swarm: msg.swarm.as_str(),
            author: msg.author.as_str(),
            ts: msg.timestamp,
            to: to.as_str(),
            exchange_id: exchange_id.as_str(),
            kind: *kind,
            done,
            total,
            display: exchange_progress_display(
                msg.author.as_str(),
                to.as_str(),
                *kind,
                done,
                total,
            ),
            is_self,
        })
        .expect("exchange_progress event serialization should never fail");
    }
    serde_json::to_string(&ExchangeLine {
        event: "exchange",
        id: msg.id.as_str(),
        swarm: msg.swarm.as_str(),
        author: msg.author.as_str(),
        pubkey: (!msg.pubkey.is_empty()).then_some(msg.pubkey.as_str()),
        ts: msg.timestamp,
        to: to.as_str(),
        exchange_id: exchange_id.as_str(),
        kind: *kind,
        phase: *phase,
        body: msg.body.as_str(),
        display: exchange_display(
            msg.author.as_str(),
            to.as_str(),
            *kind,
            *phase,
            msg.body.as_str(),
        ),
        is_self,
    })
    .expect("exchange event serialization should never fail")
}

pub(super) fn print_exchange_json(msg: &Message, is_self: bool) {
    emit(&format_exchange_json(msg, is_self));
}

/// Render a captured [`OutputEvent`] to the exact JSON line the
/// daemon writes in `--output json` mode. Reuses the same serializers
/// as the `Stream` sink, so in-process tests assert the byte-identical
/// wire format the `/swarm` skill + MCP clients parse. `None` for events
/// that produce no JSON line in JSON mode (`SwarmId` is the bare stderr
/// `ahs…` line, never JSON).
#[must_use]
pub fn event_json(event: &OutputEvent) -> Option<String> {
    let json = match event {
        OutputEvent::Ready {
            swarm,
            name,
            nickname,
            drift,
        } => serde_json::to_string(&SimpleEvent::Ready {
            version: crate::VERSION,
            swarm: swarm.as_str(),
            name: name.as_str(),
            nickname: nickname.as_str(),
            drift: drift.as_deref(),
        }),
        OutputEvent::Message { msg, is_self } => return Some(format_msg_json(msg, *is_self)),
        OutputEvent::Exchange { msg, is_self } => {
            return Some(format_exchange_json(msg, *is_self));
        }
        OutputEvent::ExchangeTimeout { exchange_id } => {
            serde_json::to_string(&SimpleEvent::ExchangeTimeout {
                exchange_id: exchange_id.as_str(),
            })
        }
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
            nickname: nickname.as_str(),
            last_seen_secs_ago: *last_seen_secs_ago,
            display: peer_timeout_display(nickname.as_str()),
        }),
        OutputEvent::PeerReturn { nickname } => serde_json::to_string(&SimpleEvent::PeerReturn {
            nickname: nickname.as_str(),
            display: peer_return_display(nickname.as_str()),
        }),
        OutputEvent::Fork {
            nickname,
            pubkey,
            seq,
        } => serde_json::to_string(&SimpleEvent::Fork {
            nickname: nickname.as_str(),
            pubkey,
            seq: *seq,
        }),
        OutputEvent::MsgPosted { id } => {
            serde_json::to_string(&SimpleEvent::MsgPosted { id: id.as_str() })
        }
        OutputEvent::Info { message } => serde_json::to_string(&SimpleEvent::Info { message }),
        OutputEvent::Error { message } => serde_json::to_string(&SimpleEvent::Error { message }),
        OutputEvent::PingReport { peers, known } => {
            serde_json::to_string(&SimpleEvent::PingReport {
                responded: peers.len(),
                display: ping_report_display(peers, *known),
                peers: peers.clone(),
                known: *known,
            })
        }
        OutputEvent::SwarmId { .. } => return None,
    };
    json.ok()
}
