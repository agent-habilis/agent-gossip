//! One line per swarm message (sent/received) on the
//! `agent_habilis_swarm::messages` tracing target, pinned always-on in
//! the default filter (`src/main.rs`). Logging only, no control flow.
//! Msg + presence joined/left at `info`; Alive/PeerInfo/Digest at
//! `trace`.
//!
//! **Body redaction.** By default the `Msg` body is replaced with
//! `<redacted NB hashprefix>` — byte length plus the first 8 hex chars of
//! [`Message::content_hash_hex`] (stable across nodes for the same
//! message, so a dev can still correlate one message across several
//! members' log files). The `--log-raw` flag opts a local debugging run
//! back into raw bodies; default `false` so any log a user collects is
//! safe to share. Authorship metadata (`dir`/`author`/`ts`/`reply`/
//! `presence`) is never redacted — it is debug, not the body.

use std::sync::OnceLock;

use crate::protocol::{Message, MessageKind, PresenceSubtype};

/// Resolved once at first log: did the operator pass `--log-raw`?
/// The CLI dispatcher installs the [`crate::util::logs::LogConfig`] before
/// any message can flow, so a stale read is structurally impossible.
static LOG_RAW: OnceLock<bool> = OnceLock::new();

fn log_raw() -> bool {
    *LOG_RAW.get_or_init(crate::util::logs::log_raw)
}

/// Redacted stand-in for a message body: byte length + the first 8 hex
/// chars of the message's content hash. The hash is identical on every
/// node for the same message (`content_hash_hex` covers the full
/// canonical/stamped form), so the prefix is a stable correlation token
/// across multiple members' log files without revealing the raw body. 8
/// hex chars = 32 bits — wide enough that a per-session collision is
/// unlikely while staying compact.
fn redacted_body(msg: &Message) -> String {
    let len = msg.body.as_str().len();
    let hash = msg.content_hash_hex();
    format!("<redacted {len}B {}>", &hash[..8])
}

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
        MessageKind::Msg { reply } => {
            if log_raw() {
                tracing::info!(
                    target: "agent_habilis_swarm::messages",
                    dir = direction,
                    author = %msg.author,
                    ts = msg.timestamp,
                    reply = ?reply,
                    body = %msg.body,
                    "msg"
                );
            } else {
                tracing::info!(
                    target: "agent_habilis_swarm::messages",
                    dir = direction,
                    author = %msg.author,
                    ts = msg.timestamp,
                    reply = ?reply,
                    body = %redacted_body(msg),
                    "msg"
                );
            }
        }
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
        MessageKind::Exchange {
            to,
            exchange_id,
            kind,
            phase,
        } => tracing::info!(
            target: "agent_habilis_swarm::messages",
            dir = direction,
            author = %msg.author,
            ts = msg.timestamp,
            to = %to,
            exchange_id = %exchange_id,
            kind = %kind,
            phase = %phase,
            body = %if log_raw() { msg.body.as_str().to_owned() } else { redacted_body(msg) },
            "exchange"
        ),
        // Plumbing — exhaustive so a new kind forces a decision.
        MessageKind::Presence {
            subtype: PresenceSubtype::Alive,
        }
        | MessageKind::PeerInfo
        | MessageKind::Digest
        | MessageKind::Ping
        | MessageKind::Pong { .. } => tracing::trace!(
            target: "agent_habilis_swarm::messages",
            dir = direction,
            author = %msg.author,
            kind = %msg.kind,
            "plumbing"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::redacted_body;
    use crate::protocol::{Message, MessageKind};

    #[test]
    fn redacted_body_omits_raw_text() {
        let secret = "totally private chat content";
        let msg = Message::fixture(MessageKind::Msg { reply: None }, secret);
        let redacted = redacted_body(&msg);
        assert!(
            !redacted.contains(secret),
            "redacted form must not leak the raw body: {redacted}"
        );
    }

    #[test]
    fn redacted_body_carries_length_and_hash_prefix() {
        let body = "hello world"; // 11 bytes
        let msg = Message::fixture(MessageKind::Msg { reply: None }, body);
        let redacted = redacted_body(&msg);

        assert!(
            redacted.starts_with("<redacted 11B "),
            "expected `<redacted 11B `, got: {redacted}"
        );
        assert!(redacted.ends_with('>'), "expected trailing `>`: {redacted}");

        // 8 hex chars between the space and the closing `>`.
        let hex = redacted
            .strip_prefix("<redacted 11B ")
            .and_then(|rest| rest.strip_suffix('>'))
            .expect("redaction layout");
        assert_eq!(hex.len(), 8, "expected 8-char hash prefix, got: {hex}");
        assert!(
            hex.chars().all(|ch| ch.is_ascii_hexdigit()),
            "expected hex, got: {hex}"
        );
    }

    #[test]
    fn redacted_body_is_deterministic_for_the_same_message() {
        let msg = Message::fixture(MessageKind::Msg { reply: None }, "same body");
        assert_eq!(redacted_body(&msg), redacted_body(&msg));
    }

    #[test]
    fn redacted_body_differs_when_content_changes() {
        let alpha = Message::fixture(MessageKind::Msg { reply: None }, "alpha");
        let beta = Message::fixture(MessageKind::Msg { reply: None }, "beta");
        // Different lengths *and* different hashes ⇒ different strings.
        assert_ne!(redacted_body(&alpha), redacted_body(&beta));
    }
}
