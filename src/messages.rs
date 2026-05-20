//! One line per swarm message (sent/received) on the
//! `agent_habilis_swarm::messages` tracing target, pinned always-on in
//! the default filter (`src/main.rs`). Logging only, no control flow.
//! Msg + presence joined/left at `info`; Alive/PeerInfo/Digest at
//! `trace`.

use crate::protocol::{Message, MessageKind, PresenceSubtype};

/// Log an inbound (received) swarm message.
pub(crate) fn log_in(msg: &Message) {
    log("in", msg);
}

/// Log an outbound (sent) swarm message.
pub(crate) fn log_out(msg: &Message) {
    log("out", msg);
}

fn log(direction: &'static str, msg: &Message) {
    match &msg.kind {
        MessageKind::Msg { reply } => tracing::info!(
            target: "agent_habilis_swarm::messages",
            dir = direction,
            author = %msg.author,
            ts = msg.timestamp,
            reply = ?reply,
            body = %msg.body,
            "msg"
        ),
        MessageKind::Presence {
            subtype: subtype @ (PresenceSubtype::Joined | PresenceSubtype::Left),
        } => tracing::info!(
            target: "agent_habilis_swarm::messages",
            dir = direction,
            author = %msg.author,
            ts = msg.timestamp,
            presence = %subtype,
            "presence"
        ),
        // Plumbing — exhaustive so a new kind forces a decision.
        MessageKind::Presence {
            subtype: PresenceSubtype::Alive,
        }
        | MessageKind::PeerInfo
        | MessageKind::Digest => tracing::trace!(
            target: "agent_habilis_swarm::messages",
            dir = direction,
            author = %msg.author,
            kind = %msg.kind,
            "plumbing"
        ),
    }
}
