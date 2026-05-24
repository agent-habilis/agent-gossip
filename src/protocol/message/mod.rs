//! The `Message` envelope + its value types.
//!
//! - [`MessageBody`] ([`body`]) and [`MessageId`] ([`id`]) — the
//!   validated newtypes the envelope carries.
//! - [`Message`] (this file) — the JSON wire envelope, its
//!   [`MessageKind`] / [`PresenceSubtype`] tags, the constructors, and
//!   (de)serialization.

use std::fmt;

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::util::clock;

use super::nickname::Nickname;
use super::swarm::SwarmId;

mod body;
mod id;

pub use body::{BodyError, MessageBody};
pub use id::{IdError, MessageId};

/// Maximum serialized message size — a network-wide wire contract kept
/// under iroh-gossip's payload budget so a message we accept always fits
/// one gossip message (see `ahs_shared::MAX_MESSAGE_SIZE` for why). Lives
/// in the shared crate; the compile-time assertion below guards the
/// relationship against the live gossip constant.
pub(crate) use ahs_shared::MAX_MESSAGE_SIZE;

/// Compile-time tripwire: a serialized message up to `MAX_MESSAGE_SIZE`
/// must fit a single iroh-gossip message, with room for gossip's
/// per-message wire overhead (header + `MessageId` + scope + length
/// prefixes, ~80B; 256 leaves margin). If our cap ever reaches gossip's
/// `DEFAULT_MAX_MESSAGE_SIZE`, oversize messages silently fail to
/// propagate (p2panda #628) — so an iroh-gossip bump that lowers the
/// limit under us fails the build here, not in production.
const _: () = assert!(
    MAX_MESSAGE_SIZE + 256 <= iroh_gossip::proto::DEFAULT_MAX_MESSAGE_SIZE,
    "MAX_MESSAGE_SIZE leaves too little room under iroh-gossip's DEFAULT_MAX_MESSAGE_SIZE"
);

/// Protocol version embedded in every message.
pub(crate) const VERSION: &str = "1.0";

/// Presence subtype.
/// `Joined`/`Left` are user-visible; `Alive` is a silent keepalive used
/// by the heartbeat-based participant tracker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PresenceSubtype {
    Joined,
    Left,
    Alive,
}

impl fmt::Display for PresenceSubtype {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PresenceSubtype::Joined => write!(f, "joined"),
            PresenceSubtype::Left => write!(f, "left"),
            PresenceSubtype::Alive => write!(f, "alive"),
        }
    }
}

/// Message kind — three types cover all protocol needs:
/// - `Msg`: content. `reply: None` = open message, `reply: Some(nick)` = directed at a peer.
/// - `Presence`: agent lifecycle (joined/left), empty body, no `reply`.
/// - `PeerInfo`: infrastructure — carries endpoint address for mesh formation. Not user-visible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum MessageKind {
    Msg {
        #[serde(skip_serializing_if = "Option::is_none")]
        reply: Option<Nickname>,
    },
    Presence {
        subtype: PresenceSubtype,
    },
    PeerInfo,
    /// Anti-entropy digest. Body is a JSON array of recent message ids
    /// the sender holds; a receiver re-broadcasts any of *its* logged
    /// messages absent from that list, so a peer that missed them
    /// (partition / sleep / late join) recovers. Plumbing like
    /// `PeerInfo`: never rate-limited, logged, or surfaced via
    /// `poll`/`fetch`.
    Digest,
    /// Liveness probe broadcast by a node running an RTT round. Every
    /// receiver auto-responds with a `Pong` addressed back to the
    /// pinger. Plumbing like `PeerInfo`/`Digest`: never rate-limited,
    /// logged, or surfaced via `poll`/`fetch` — only the originator's
    /// `ping_report` event surfaces.
    Ping,
    /// Response to a `Ping`, addressed to the original pinger (`to`).
    /// The pinger records its local arrival time to compute RTT. Same
    /// plumbing treatment as `Ping`.
    Pong {
        to: Nickname,
    },
}

impl fmt::Display for MessageKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MessageKind::Msg { .. } => write!(f, "msg"),
            MessageKind::Presence { .. } => write!(f, "presence"),
            MessageKind::PeerInfo => write!(f, "peerinfo"),
            MessageKind::Digest => write!(f, "digest"),
            MessageKind::Ping => write!(f, "ping"),
            MessageKind::Pong { .. } => write!(f, "pong"),
        }
    }
}

fn default_ext() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

fn empty_body() -> MessageBody {
    MessageBody::new("").expect("empty string is always a valid MessageBody")
}

/// A protocol message — serialized as JSON on the wire.
///
/// Wire format (compact JSON, one line):
/// ```json
/// {"v":"1.0","id":"<uuid>","type":"msg","swarm":"ahs...","author":"word-word","ts":1234567890,"body":"text","ext":{}}
/// ```
///
/// `reply` (the addressee nickname) is inlined into the JSON for directed `msg` kinds.
/// `ext`: free-form object for experimental/future fields; parsers MUST ignore unknown keys inside it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    #[serde(rename = "v")]
    pub version: String,
    pub id: MessageId,
    #[serde(flatten)]
    pub kind: MessageKind,
    pub swarm: SwarmId,
    pub author: Nickname,
    /// Unix timestamp (seconds, UTC).
    #[serde(rename = "ts")]
    pub timestamp: i64,
    pub body: MessageBody,
    /// Extension escape hatch. Add experimental fields here; stable fields get promoted to top-level.
    #[serde(default = "default_ext")]
    pub ext: serde_json::Value,
}

impl Message {
    fn new(swarm: &SwarmId, author: &Nickname, kind: MessageKind, body: MessageBody) -> Self {
        Message {
            version: VERSION.to_string(),
            id: MessageId::random(),
            kind,
            swarm: swarm.clone(),
            author: author.clone(),
            timestamp: clock::unix_secs(),
            body,
            ext: default_ext(),
        }
    }

    pub(crate) fn new_message(swarm: &SwarmId, author: &Nickname, body: MessageBody) -> Self {
        Self::new(swarm, author, MessageKind::Msg { reply: None }, body)
    }

    pub(crate) fn new_joined(swarm: &SwarmId, author: &Nickname) -> Self {
        Self::new(
            swarm,
            author,
            MessageKind::Presence {
                subtype: PresenceSubtype::Joined,
            },
            empty_body(),
        )
    }

    pub(crate) fn new_left(swarm: &SwarmId, author: &Nickname) -> Self {
        Self::new(
            swarm,
            author,
            MessageKind::Presence {
                subtype: PresenceSubtype::Left,
            },
            empty_body(),
        )
    }

    pub(crate) fn new_alive(swarm: &SwarmId, author: &Nickname) -> Self {
        Self::new(
            swarm,
            author,
            MessageKind::Presence {
                subtype: PresenceSubtype::Alive,
            },
            empty_body(),
        )
    }

    pub(crate) fn new_reply(
        swarm: &SwarmId,
        author: &Nickname,
        reply: Nickname,
        body: MessageBody,
    ) -> Self {
        Self::new(swarm, author, MessageKind::Msg { reply: Some(reply) }, body)
    }

    /// A liveness probe (broadcast). Receivers auto-respond with a
    /// `Pong` addressed back to `author`.
    pub(crate) fn new_ping(swarm: &SwarmId, author: &Nickname) -> Self {
        Self::new(swarm, author, MessageKind::Ping, empty_body())
    }

    /// A `Pong` response addressed to the original pinger (`to`).
    pub(crate) fn new_pong(swarm: &SwarmId, author: &Nickname, to: Nickname) -> Self {
        Self::new(swarm, author, MessageKind::Pong { to }, empty_body())
    }

    /// Create a `PeerInfo` message. The body carries endpoint address data
    /// as a JSON string for mesh peer lookup.
    pub(crate) fn new_peer_info(
        swarm: &SwarmId,
        author: &Nickname,
        addr_data: MessageBody,
    ) -> Self {
        Self::new(swarm, author, MessageKind::PeerInfo, addr_data)
    }

    /// An anti-entropy digest carrying `ids_json` (a JSON array of the
    /// recent message ids we hold) in the body.
    pub(crate) fn new_digest(swarm: &SwarmId, author: &Nickname, ids_json: MessageBody) -> Self {
        Self::new(swarm, author, MessageKind::Digest, ids_json)
    }

    /// Serialize to compact JSON bytes for the gossip wire.
    pub(crate) fn serialize(&self) -> Result<Vec<u8>> {
        let bytes = serde_json::to_vec(self).context("failed to serialize message")?;
        if bytes.len() > MAX_MESSAGE_SIZE {
            bail!(
                "message too large: {} bytes (max {})",
                bytes.len(),
                MAX_MESSAGE_SIZE
            );
        }
        Ok(bytes)
    }

    /// Parse a message from JSON bytes.
    pub(crate) fn parse(data: &[u8]) -> Result<Self> {
        if data.len() > MAX_MESSAGE_SIZE {
            bail!("message too large");
        }
        let msg: Message = serde_json::from_slice(data).context("failed to parse message JSON")?;
        if msg.version != VERSION {
            bail!("unsupported protocol version: {}", msg.version);
        }
        Ok(msg)
    }
}

/// Build the serialized wire bytes for an outbound user message
/// (open or directed reply), returning the bytes alongside the
/// canonical [`Message`] so callers can echo it without re-parsing.
/// The single message-construction point shared by the IPC `msg`
/// command, the embed send path, and interactive stdin.
///
/// # Errors
/// Propagates [`Message::serialize`] failure (oversized payload).
pub(crate) fn build_msg_bytes(
    swarm: &SwarmId,
    body: MessageBody,
    reply: Option<Nickname>,
    author: &Nickname,
) -> Result<(Bytes, Message)> {
    let msg = match reply {
        None => Message::new_message(swarm, author, body),
        Some(target) => Message::new_reply(swarm, author, target, body),
    };
    let raw = msg.serialize()?;
    Ok((Bytes::from(raw), msg))
}

#[cfg(test)]
impl Message {
    pub(crate) fn fixture(kind: MessageKind, body: &str) -> Self {
        Message {
            version: "1.0".into(),
            id: "00000000-0000-0000-0000-000000000001".into(),
            kind,
            swarm: SwarmId::from("ahstest"),
            author: "alice-bot".into(),
            timestamp: 1_700_000_000,
            body: body.into(),
            ext: serde_json::json!({}),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Message, MessageBody, MessageKind, Nickname, PresenceSubtype, SwarmId, build_msg_bytes,
    };

    fn nick(name: &str) -> Nickname {
        Nickname::from(name)
    }

    fn sid() -> SwarmId {
        SwarmId::from("ahstest")
    }

    #[test]
    fn test_round_trip() {
        let msg = Message::new_message(
            &sid(),
            &nick("word-word"),
            MessageBody::from("Hello, world!"),
        );
        let bytes = msg.serialize().unwrap();
        let parsed = Message::parse(&bytes).unwrap();
        assert_eq!(parsed.id, msg.id);
        assert_eq!(parsed.kind, MessageKind::Msg { reply: None });
        assert_eq!(parsed.body, msg.body);
    }

    #[test]
    fn test_reply_round_trip() {
        let msg = Message::new_message(&sid(), &nick("word-word"), MessageBody::from("A message?"));
        let reply = Message::new_reply(
            &sid(),
            &nick("other-nick"),
            msg.author.clone(),
            MessageBody::from("A reply."),
        );
        let bytes = reply.serialize().unwrap();
        let parsed = Message::parse(&bytes).unwrap();
        assert_eq!(
            parsed.kind,
            MessageKind::Msg {
                reply: Some(msg.author)
            }
        );
    }

    #[test]
    fn test_alive_round_trip() {
        let msg = Message::new_alive(&sid(), &nick("word-word"));
        let bytes = msg.serialize().unwrap();
        let parsed = Message::parse(&bytes).unwrap();
        assert_eq!(
            parsed.kind,
            MessageKind::Presence {
                subtype: PresenceSubtype::Alive
            }
        );
        assert_eq!(parsed.body.as_str(), "");
    }

    #[test]
    fn test_ping_round_trip() {
        let msg = Message::new_ping(&sid(), &nick("word-word"));
        let bytes = msg.serialize().unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("\"type\":\"ping\""));
        let parsed = Message::parse(&bytes).unwrap();
        assert_eq!(parsed.kind, MessageKind::Ping);
        assert_eq!(parsed.body.as_str(), "");
    }

    #[test]
    fn test_pong_round_trip() {
        let target = nick("pinger-here");
        let msg = Message::new_pong(&sid(), &nick("word-word"), target.clone());
        let bytes = msg.serialize().unwrap();
        let parsed = Message::parse(&bytes).unwrap();
        assert_eq!(parsed.kind, MessageKind::Pong { to: target });
        assert_eq!(parsed.body.as_str(), "");
    }

    #[test]
    fn test_ext_round_trip() {
        let mut msg =
            Message::new_message(&sid(), &nick("word-word"), MessageBody::from("With ext."));
        msg.ext = serde_json::json!({"tags": ["rust", "p2p"], "priority": 1});
        let bytes = msg.serialize().unwrap();
        let parsed = Message::parse(&bytes).unwrap();
        assert_eq!(parsed.ext["tags"][0], "rust");
        assert_eq!(parsed.ext["priority"], 1);
    }

    #[test]
    fn test_unknown_ext_fields_ignored() {
        let json = r#"{"v":"1.0","id":"abc","type":"msg","swarm":"ahstest","author":"a-b","ts":0,"body":"hi","ext":{"future_field":"value","another":42}}"#;
        let parsed = Message::parse(json.as_bytes()).unwrap();
        assert_eq!(parsed.body.as_str(), "hi");
        assert_eq!(parsed.ext["future_field"], "value");
    }

    #[test]
    fn test_missing_ext_defaults_to_empty_object() {
        let json = r#"{"v":"1.0","id":"abc","type":"msg","swarm":"ahstest","author":"a-b","ts":0,"body":"hi"}"#;
        let parsed = Message::parse(json.as_bytes()).unwrap();
        assert_eq!(parsed.ext, serde_json::json!({}));
    }

    #[test]
    fn test_version_mismatch_rejected() {
        let json = r#"{"v":"2.0","id":"abc","type":"msg","swarm":"ahstest","author":"a-b","ts":0,"body":"hi","ext":{}}"#;
        assert!(Message::parse(json.as_bytes()).is_err());
    }

    #[test]
    fn build_msg_bytes_message() {
        let alice = nick("alice");
        let (bytes, built) =
            build_msg_bytes(&sid(), MessageBody::from("hello"), None, &alice).unwrap();
        assert!(!built.id.as_str().is_empty());
        assert!(!bytes.is_empty());
        let msg = Message::parse(&bytes).unwrap();
        assert_eq!(msg.body.as_str(), "hello");
        assert_eq!(msg.author, alice);
    }

    #[test]
    fn build_msg_bytes_reply() {
        let target = nick("alice");
        let bob = nick("bob");
        let (bytes, _) = build_msg_bytes(
            &sid(),
            MessageBody::from("reply"),
            Some(target.clone()),
            &bob,
        )
        .unwrap();
        let msg = Message::parse(&bytes).unwrap();
        assert_eq!(msg.body.as_str(), "reply");
        match msg.kind {
            MessageKind::Msg { reply } => assert_eq!(reply, Some(target)),
            MessageKind::Presence { .. }
            | MessageKind::PeerInfo
            | MessageKind::Digest
            | MessageKind::Ping
            | MessageKind::Pong { .. } => {
                panic!("expected Msg kind")
            }
        }
    }

    mod snapshots {
        use super::{Message, MessageKind, Nickname, PresenceSubtype};

        #[test]
        fn snap_wire_message() {
            let msg = Message::fixture(MessageKind::Msg { reply: None }, "What is Rust?");
            let bytes = msg.serialize().unwrap();
            let wire = String::from_utf8(bytes).unwrap();
            insta::assert_snapshot!(wire);
        }

        #[test]
        fn snap_wire_reply() {
            let msg = Message::fixture(
                MessageKind::Msg {
                    reply: Some(Nickname::from("addressed-nick")),
                },
                "Rust is a systems language.",
            );
            let bytes = msg.serialize().unwrap();
            let wire = String::from_utf8(bytes).unwrap();
            insta::assert_snapshot!(wire);
        }

        #[test]
        fn snap_wire_presence_joined() {
            let msg = Message::fixture(
                MessageKind::Presence {
                    subtype: PresenceSubtype::Joined,
                },
                "",
            );
            let bytes = msg.serialize().unwrap();
            let wire = String::from_utf8(bytes).unwrap();
            insta::assert_snapshot!(wire);
        }
    }

    mod prop {
        use proptest::{
            collection::vec as arb_vec, prelude::any, prop_assert, prop_assert_eq, proptest,
            strategy::Strategy,
        };

        use super::super::{
            MAX_MESSAGE_SIZE, Message, MessageBody, MessageKind, Nickname, VERSION,
        };
        use super::sid;

        fn arb_ascii_body() -> impl Strategy<Value = String> {
            arb_vec(0x20u8..0x7Eu8, 0..200).prop_map(|bytes| String::from_utf8(bytes).unwrap())
        }

        fn arb_nickname() -> impl Strategy<Value = Nickname> {
            "[a-z]{3,8}-[a-z]{3,8}".prop_map(|raw| Nickname::new(raw).unwrap())
        }

        proptest! {
            #[test]
            fn prop_message_round_trip(
                body in arb_ascii_body(),
                author in arb_nickname(),
            ) {
                let body = MessageBody::new(body).unwrap();
                let msg = Message::new_message(&sid(), &author, body);
                let bytes = msg.serialize().unwrap();
                let parsed = Message::parse(&bytes).unwrap();
                prop_assert_eq!(&parsed.body, &msg.body);
                prop_assert_eq!(&parsed.author, &msg.author);
                prop_assert_eq!(&parsed.version, VERSION);
                prop_assert_eq!(parsed.kind, MessageKind::Msg { reply: None });
            }

            #[test]
            fn prop_reply_round_trip(
                body in arb_ascii_body(),
                author in arb_nickname(),
                target in arb_nickname(),
            ) {
                let body = MessageBody::new(body).unwrap();
                let expected_body = body.clone();
                let expected_target = target.clone();
                let msg = Message::new_reply(&sid(), &author, target, body);
                let bytes = msg.serialize().unwrap();
                let parsed = Message::parse(&bytes).unwrap();
                prop_assert_eq!(&parsed.body, &expected_body);
                prop_assert_eq!(
                    parsed.kind,
                    MessageKind::Msg { reply: Some(expected_target) }
                );
            }

            #[test]
            fn prop_presence_round_trip(is_join in any::<bool>()) {
                let test_nick = Nickname::from("test-nick");
                let msg = if is_join {
                    Message::new_joined(&sid(), &test_nick)
                } else {
                    Message::new_left(&sid(), &test_nick)
                };
                let bytes = msg.serialize().unwrap();
                let parsed = Message::parse(&bytes).unwrap();
                prop_assert_eq!(parsed.kind, msg.kind);
            }

            #[test]
            fn prop_control_chars_rejected(
                // C0 controls excluding the allowed tab/newline/cr.
                body in "[\\x00-\\x08\\x0b\\x0c\\x0e-\\x1f]{1,10}",
            ) {
                prop_assert!(MessageBody::new(body).is_err());
            }

            #[test]
            fn prop_unicode_body_round_trip(
                // `\P{C}` excludes every category-C scalar (all
                // controls included), so `new` can't reject here and the
                // `.unwrap()` is safe. This fuzzes the multibyte round-trip.
                body in "\\P{C}{0,50}",
                author in arb_nickname(),
            ) {
                let body = MessageBody::new(body).unwrap();
                let expected = body.clone();
                let msg = Message::new_message(&sid(), &author, body);
                let bytes = msg.serialize().unwrap();
                let parsed = Message::parse(&bytes).unwrap();
                prop_assert_eq!(&parsed.body, &expected);
            }

            #[test]
            fn prop_serialized_size_within_limit(
                body in arb_ascii_body(),
            ) {
                let msg = Message::new_message(
                    &sid(),
                    &Nickname::from("nick-name"),
                    MessageBody::new(body).unwrap(),
                );
                if let Ok(bytes) = msg.serialize() {
                    prop_assert!(bytes.len() <= MAX_MESSAGE_SIZE);
                }
            }
        }
    }
}
