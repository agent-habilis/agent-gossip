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

use super::identity::{self, Identity};
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
    /// Author's Ed25519 public key (lowercase hex), and the detached
    /// signature over the message's [canonical bytes](Message::canonical_bytes).
    /// Empty on an unsigned message (which then serializes exactly as a v1
    /// message — the fields are skipped); real outbound traffic is signed on
    /// the broadcast path. See [`docs/history-integrity.md`].
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pubkey: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sig: String,
    /// Per-author hash-linked log (Phase 2), set on `Msg` only: `seq` is
    /// this author's monotonic counter and `prev` the content hash of their
    /// previous `Msg` (`None` at `seq 0`). Both are signed. Plumbing /
    /// presence kinds leave them `None`. The message's own content hash is
    /// computed locally (`content_hash_hex`), never transmitted. See
    /// [`docs/history-integrity.md`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev: Option<String>,
    /// Cross-author DAG (Phase 3), `Msg` only: content hashes of the DAG
    /// tips this author had seen when authoring — the causal links. Signed.
    /// Empty for the very first message / messages with no observed
    /// predecessor. See [`docs/history-integrity.md`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parents: Vec<String>,
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
            pubkey: String::new(),
            sig: String::new(),
            seq: None,
            prev: None,
            parents: Vec::new(),
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

    /// Deterministic, domain-separated, length-prefixed encoding of every
    /// signed field (i.e. all of them **except** `sig`, including
    /// `pubkey` so the key is bound to the message). This — not the JSON —
    /// is what gets signed and hashed, so signature verification does not
    /// depend on JSON formatting. `kind` and `ext` are folded in via their
    /// (deterministic, sorted-key) `serde_json` encodings.
    #[must_use]
    pub(crate) fn canonical_bytes(&self) -> Vec<u8> {
        const DOMAIN: &[u8] = b"agent-habilis-swarm/msg";
        let mut buf = Vec::new();
        let mut field = |bytes: &[u8]| {
            buf.extend_from_slice(
                &u32::try_from(bytes.len())
                    .expect("a message field fits in u32")
                    .to_le_bytes(),
            );
            buf.extend_from_slice(bytes);
        };
        field(DOMAIN);
        field(self.version.as_bytes());
        field(self.id.as_str().as_bytes());
        field(&serde_json::to_vec(&self.kind).unwrap_or_default());
        field(self.swarm.as_str().as_bytes());
        field(self.author.as_str().as_bytes());
        field(&self.timestamp.to_le_bytes());
        field(self.body.as_str().as_bytes());
        field(self.pubkey.as_bytes());
        // `seq`/`prev` are signed too. `None` encodes as a zero-length field
        // (distinct from `Some(0)`, which is 8 bytes), so a plumbing message
        // and a `seq 0` message never collide.
        match self.seq {
            Some(value) => field(&value.to_le_bytes()),
            None => field(&[]),
        }
        match &self.prev {
            Some(value) => field(value.as_bytes()),
            None => field(&[]),
        }
        // Parents (DAG causal links) are signed too. Serialized as their
        // deterministic JSON array; empty for no-parent messages.
        field(&serde_json::to_vec(&self.parents).unwrap_or_default());
        field(&serde_json::to_vec(&self.ext).unwrap_or_default());
        buf
    }

    /// This message's content hash (SHA-256 of [`canonical_bytes`], hex) —
    /// the id used by another author's `prev` backlink and by fork
    /// detection. Recomputed locally on receive; never trusted off the wire.
    #[must_use]
    pub(crate) fn content_hash_hex(&self) -> String {
        identity::content_hash_hex(&self.canonical_bytes())
    }

    /// Stamp the per-author log fields before signing (`Msg` only). `seq`
    /// is the author's monotonic counter, `prev` the hash of their previous
    /// `Msg` (`None` at `seq 0`). Consuming-builder so it composes with
    /// [`signed`](Self::signed): `Message::new_message(..).with_chain(..).signed(..)`.
    #[must_use]
    pub(crate) fn with_chain(mut self, seq: u64, prev: Option<String>) -> Self {
        self.seq = Some(seq);
        self.prev = prev;
        self
    }

    /// Stamp the cross-author DAG `parents` (content hashes of the tips
    /// seen when authoring) before signing. Consuming-builder, composes
    /// with [`with_chain`](Self::with_chain) and [`signed`](Self::signed).
    #[must_use]
    pub(crate) fn with_parents(mut self, parents: Vec<String>) -> Self {
        self.parents = parents;
        self
    }

    /// Sign this message with `identity`, filling `pubkey` then `sig`.
    /// `pubkey` is set before the canonical bytes are computed so the key
    /// is part of what is signed; consuming-builder style so it composes in
    /// the construction expression (`Message::new_message(..).signed(&id)`).
    #[must_use]
    pub(crate) fn signed(mut self, identity: &Identity) -> Self {
        self.pubkey = identity::encode_pubkey(&identity.public());
        self.sig = identity::encode_sig(&identity.sign(&self.canonical_bytes()));
        self
    }

    /// Verify the detached signature against the embedded `pubkey` over the
    /// canonical bytes. `false` if either field is absent/malformed or the
    /// signature does not match — never panics. (Whether `pubkey` is the
    /// *expected* identity for this `author` is the receiver's TOFU check,
    /// separate from this cryptographic check.)
    #[must_use]
    pub(crate) fn verify_signature(&self) -> bool {
        if self.pubkey.is_empty() || self.sig.is_empty() {
            return false;
        }
        let (Ok(pubkey), Ok(sig)) = (
            identity::decode_pubkey(&self.pubkey),
            identity::decode_sig(&self.sig),
        ) else {
            return false;
        };
        identity::verify(&pubkey, &self.canonical_bytes(), &sig)
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
/// The per-author log + DAG position to stamp on an outbound `Msg`: the
/// chain `seq`/`prev` (Phase 2) and the DAG `parents` (Phase 3). Bundled so
/// [`build_msg_bytes`] stays within the argument budget; the daemon fills it
/// from its send cursor + current DAG tips.
pub(crate) struct ChainCtx {
    pub seq: u64,
    pub prev: Option<String>,
    pub parents: Vec<String>,
}

#[cfg(test)]
impl ChainCtx {
    /// The genesis position (seq 0, no predecessor, no parents).
    pub(crate) fn genesis() -> Self {
        ChainCtx {
            seq: 0,
            prev: None,
            parents: Vec::new(),
        }
    }
}

pub(crate) fn build_msg_bytes(
    swarm: &SwarmId,
    body: MessageBody,
    reply: Option<Nickname>,
    author: &Nickname,
    identity: &Identity,
    chain: ChainCtx,
) -> Result<(Bytes, Message)> {
    let msg = match reply {
        None => Message::new_message(swarm, author, body),
        Some(target) => Message::new_reply(swarm, author, target, body),
    }
    .with_chain(chain.seq, chain.prev)
    .with_parents(chain.parents)
    .signed(identity);
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
            pubkey: String::new(),
            sig: String::new(),
            seq: None,
            prev: None,
            parents: Vec::new(),
            ext: serde_json::json!({}),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ChainCtx, Message, MessageBody, MessageKind, Nickname, PresenceSubtype, SwarmId,
        build_msg_bytes,
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
        let identity = crate::protocol::identity::Identity::generate();
        let (bytes, built) = build_msg_bytes(
            &sid(),
            MessageBody::from("hello"),
            None,
            &alice,
            &identity,
            ChainCtx::genesis(),
        )
        .unwrap();
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
        let identity = crate::protocol::identity::Identity::generate();
        let (bytes, _) = build_msg_bytes(
            &sid(),
            MessageBody::from("reply"),
            Some(target.clone()),
            &bob,
            &identity,
            ChainCtx::genesis(),
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

    mod signing {
        use super::super::{Message, MessageKind};
        use crate::protocol::identity::Identity;

        fn identity() -> Identity {
            Identity::generate()
        }

        #[test]
        fn signed_message_verifies() {
            let msg =
                Message::fixture(MessageKind::Msg { reply: None }, "hello").signed(&identity());
            assert!(!msg.pubkey.is_empty() && !msg.sig.is_empty());
            assert!(msg.verify_signature());
        }

        #[test]
        fn unsigned_message_does_not_verify() {
            let msg = Message::fixture(MessageKind::Msg { reply: None }, "hello");
            assert!(!msg.verify_signature(), "empty pubkey/sig must not verify");
        }

        #[test]
        fn tampered_body_breaks_signature() {
            let mut msg =
                Message::fixture(MessageKind::Msg { reply: None }, "hello").signed(&identity());
            msg.body = "tampered".into();
            assert!(!msg.verify_signature());
        }

        #[test]
        fn tampered_author_breaks_signature() {
            let mut msg =
                Message::fixture(MessageKind::Msg { reply: None }, "hello").signed(&identity());
            msg.author = "impostor-bot".into();
            assert!(!msg.verify_signature());
        }

        #[test]
        fn signature_survives_wire_round_trip() {
            let msg = Message::fixture(MessageKind::Msg { reply: None }, "hi").signed(&identity());
            let parsed = Message::parse(&msg.serialize().unwrap()).unwrap();
            assert!(parsed.verify_signature());
            assert_eq!(parsed.pubkey, msg.pubkey);
        }

        #[test]
        fn unsigned_wire_omits_signature_fields() {
            // The skip-if-empty fields keep an unsigned message byte-identical
            // to the v1 wire, so existing snapshots are unaffected.
            let bytes = Message::fixture(MessageKind::Msg { reply: None }, "hi")
                .serialize()
                .unwrap();
            let wire = String::from_utf8(bytes).unwrap();
            assert!(!wire.contains("pubkey"), "{wire}");
            assert!(!wire.contains("\"sig\""), "{wire}");
        }
    }

    mod chain {
        use super::super::{Message, MessageKind};
        use crate::protocol::identity::Identity;

        fn msg(body: &str) -> Message {
            Message::fixture(MessageKind::Msg { reply: None }, body)
        }

        #[test]
        fn chained_message_carries_seq_prev_and_verifies() {
            let prev = "a".repeat(64);
            let signed = msg("hi")
                .with_chain(5, Some(prev.clone()))
                .signed(&Identity::generate());
            assert_eq!(signed.seq, Some(5));
            assert_eq!(signed.prev.as_deref(), Some(prev.as_str()));
            assert!(signed.verify_signature());
        }

        #[test]
        fn content_hash_is_stable_and_64_hex() {
            let stamped = msg("x").with_chain(0, None);
            assert_eq!(stamped.content_hash_hex(), stamped.content_hash_hex());
            assert_eq!(stamped.content_hash_hex().len(), 64);
        }

        #[test]
        fn fork_pair_hashes_differently() {
            // The equivocation primitive: two different messages at the same
            // seq hash differently, so a receiver can prove the fork.
            let alpha = msg("alpha").with_chain(1, None);
            let beta = msg("beta").with_chain(1, None);
            assert_ne!(alpha.content_hash_hex(), beta.content_hash_hex());
        }

        #[test]
        fn tampering_seq_breaks_signature() {
            let mut signed = msg("x").with_chain(3, None).signed(&Identity::generate());
            signed.seq = Some(4);
            assert!(!signed.verify_signature(), "seq is a signed field");
        }

        #[test]
        fn parents_are_signed_and_round_trip() {
            let parents = vec!["a".repeat(64), "b".repeat(64)];
            let signed = msg("hi")
                .with_chain(1, None)
                .with_parents(parents.clone())
                .signed(&Identity::generate());
            assert_eq!(signed.parents, parents);
            let parsed = Message::parse(&signed.serialize().unwrap()).unwrap();
            assert_eq!(parsed.parents, parents);
            assert!(parsed.verify_signature());
        }

        #[test]
        fn tampering_parents_breaks_signature() {
            let mut signed = msg("hi")
                .with_parents(vec!["a".repeat(64)])
                .signed(&Identity::generate());
            signed.parents.push("b".repeat(64));
            assert!(!signed.verify_signature(), "parents are signed");
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
