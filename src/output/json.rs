//! The JSON wire layer: the serde shapes for every line the daemon
//! emits on stdout (consumed by the `/swarm` skill and any `--output
//! json` client) plus the serializers that render them. Field
//! order/naming is part of the wire format — documented in AGENTS.md,
//! pinned by the insta snapshots in `tests`. The `Output` sink in the
//! parent module renders through these so the captured-event and
//! stdout forms stay byte-identical.

use std::io::Write;

use serde::Serialize;

use super::OutputEvent;
use crate::protocol::{Message, MessageKind, Nickname, PresenceSubtype};

/// One-shot events (everything except the `"event":"message"` family).
/// `#[serde(tag = "event")]` inlines the discriminator as the first field.
#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(super) enum SimpleEvent<'a> {
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
    PingReport {
        peers: Vec<PingPeer>,
        responded: usize,
        known: usize,
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
            is_self,
        })
        .expect("message event serialization should never fail"),
        MessageKind::Presence { .. }
        | MessageKind::PeerInfo
        | MessageKind::Digest
        | MessageKind::Ping
        | MessageKind::Pong { .. } => {
            unreachable!("format_msg_json only handles Msg")
        }
    }
}

pub(super) fn print_message_json(msg: &Message, is_self: bool) {
    emit(&format_msg_json(msg, is_self));
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
        } => serde_json::to_string(&SimpleEvent::Ready {
            swarm: swarm.as_str(),
            name: name.as_str(),
            nickname: nickname.as_str(),
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
            nickname: nickname.as_str(),
            last_seen_secs_ago: *last_seen_secs_ago,
        }),
        OutputEvent::PeerReturn { nickname } => {
            serde_json::to_string(&SimpleEvent::PeerReturn {
                nickname: nickname.as_str(),
            })
        }
        OutputEvent::MsgPosted { id } => {
            serde_json::to_string(&SimpleEvent::MsgPosted { id: id.as_str() })
        }
        OutputEvent::Info { message } => serde_json::to_string(&SimpleEvent::Info { message }),
        OutputEvent::Error { message } => serde_json::to_string(&SimpleEvent::Error { message }),
        OutputEvent::PingReport { peers, known } => {
            serde_json::to_string(&SimpleEvent::PingReport {
                responded: peers.len(),
                peers: peers.clone(),
                known: *known,
            })
        }
        OutputEvent::SwarmId { .. } => return None,
    };
    json.ok()
}
