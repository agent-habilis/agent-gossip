use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::util::clock;

use super::nickname::Nickname;
use super::swarm::SwarmId;

/// Maximum message size in bytes (16KB). Changing this is an
/// interop-breaking wire change; behavioural knobs live in
/// `crate::util::tuning`.
pub(crate) const MAX_MESSAGE_SIZE: usize = 16 * 1024;

// ── MessageBody ──────────────────────────────────────────────────

/// A protocol message body — UTF-8 text. Newlines and tabs are allowed
/// (multi-line snippets); other control characters are rejected. Empty
/// is legal: presence and `PeerInfo` messages use it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MessageBody(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyError(String);

impl fmt::Display for BodyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "message body must not contain control characters other than tab/newline, got {:?}",
            self.0
        )
    }
}

impl std::error::Error for BodyError {}

impl MessageBody {
    /// Construct a body. Accepts any UTF-8 text; the only restriction is
    /// control characters other than `\t`/`\n`/`\r`.
    ///
    /// # Errors
    /// Returns [`BodyError`] if `value` contains a disallowed control
    /// character (e.g. NUL or other C0/C1 controls).
    pub fn new(value: impl Into<String>) -> Result<Self, BodyError> {
        let value = value.into();
        if value
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t' | '\r'))
        {
            return Err(BodyError(value));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MessageBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for MessageBody {
    type Err = BodyError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::new(text)
    }
}

impl AsRef<str> for MessageBody {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl AsRef<[u8]> for MessageBody {
    fn as_ref(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

#[cfg(test)]
impl From<&str> for MessageBody {
    fn from(text: &str) -> Self {
        Self::new(text).expect("invalid message body in test fixture")
    }
}

#[cfg(test)]
mod body_tests {
    use super::MessageBody;

    #[test]
    fn new_accepts_ascii() {
        MessageBody::new("hello world").unwrap();
        MessageBody::new("").unwrap();
        MessageBody::new("special chars: !@#$%^&*()").unwrap();
    }

    #[test]
    fn new_accepts_unicode() {
        MessageBody::new("héllo").unwrap();
        MessageBody::new("emoji 🎉").unwrap();
        MessageBody::new("日本語のメッセージ").unwrap();
    }

    #[test]
    fn new_accepts_newline_and_tab() {
        MessageBody::new("line one\nline two").unwrap();
        MessageBody::new("col1\tcol2").unwrap();
        MessageBody::new("crlf\r\nline").unwrap();
    }

    #[test]
    fn new_rejects_control_chars() {
        assert!(MessageBody::new("nul\0byte").is_err());
        assert!(MessageBody::new("bell\u{7}char").is_err());
    }

    #[test]
    fn serde_transparent_round_trip() {
        let body = MessageBody::from("hello");
        let json = serde_json::to_string(&body).unwrap();
        assert_eq!(json, "\"hello\"");
        let parsed: MessageBody = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, body);
    }
}

// ── MessageId ────────────────────────────────────────────────────

/// A protocol message identifier — UUID v4 string form.
///
/// Construction goes through `new` (validates UUID format) or `random`
/// (mints a fresh v4). The newtype prevents argument-order confusion
/// between `id` and `after`-cursor parameters that carry the same kind
/// of value.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MessageId(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdError(String);

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid message id: {:?}", self.0)
    }
}

impl std::error::Error for IdError {}

impl MessageId {
    pub(crate) fn new(value: impl Into<String>) -> Result<Self, IdError> {
        let value = value.into();
        Uuid::parse_str(&value).map_err(|_| IdError(value.clone()))?;
        Ok(Self(value))
    }

    pub(crate) fn random() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for MessageId {
    type Err = IdError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::new(text)
    }
}

impl AsRef<str> for MessageId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for MessageId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
impl From<&str> for MessageId {
    fn from(text: &str) -> Self {
        Self::new(text).expect("invalid message id in test fixture")
    }
}

#[cfg(test)]
mod id_tests {
    use super::MessageId;

    #[test]
    fn new_accepts_uuid_v4() {
        let id = MessageId::new("550e8400-e29b-41d4-a716-446655440000").unwrap();
        assert_eq!(id.as_str(), "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn new_rejects_garbage() {
        assert!(MessageId::new("not-a-uuid").is_err());
        assert!(MessageId::new("").is_err());
        assert!(MessageId::new("550e8400").is_err());
    }

    #[test]
    fn random_produces_valid_id() {
        let first = MessageId::random();
        let second = MessageId::random();
        assert_ne!(first, second);
        MessageId::new(first.as_str()).expect("random must round-trip through new");
    }

    #[test]
    fn from_str_works_for_clap() {
        let id: MessageId = "550e8400-e29b-41d4-a716-446655440000".parse().unwrap();
        assert_eq!(id.as_str(), "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn serde_transparent_round_trip() {
        let id = MessageId::random();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{}\"", id.as_str()));
        let parsed: MessageId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, id);
    }
}

// ── Message ──────────────────────────────────────────────────────

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
}

impl fmt::Display for MessageKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MessageKind::Msg { .. } => write!(f, "msg"),
            MessageKind::Presence { .. } => write!(f, "presence"),
            MessageKind::PeerInfo => write!(f, "peerinfo"),
            MessageKind::Digest => write!(f, "digest"),
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

    /// Create a `PeerInfo` message. The body carries endpoint address data
    /// as a JSON string for mesh peer discovery.
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
    use super::*;

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
            _ => panic!("expected Msg kind"),
        }
    }

    mod snapshots {
        use super::*;

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
        use super::*;
        use proptest::collection::vec as arb_vec;
        use proptest::prelude::*;

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
