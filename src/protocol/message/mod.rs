//! The `Message` envelope + its value types.
//!
//! - [`MessageBody`] ([`body`]) and [`MessageId`] ([`id`]) — the
//!   validated newtypes the envelope carries.
//! - [`Message`] (this file) — the JSON wire envelope, its
//!   [`MessageKind`] / [`PresenceSubtype`] tags, the constructors, and
//!   (de)serialization.

use std::fmt;

use anyhow::{Context, Result, bail};
#[cfg(test)]
use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::util::clock;

use super::identity::{self, Identity};
use super::nickname::Nickname;
use super::swarm::SwarmId;

mod body;
mod id;
mod shard;

pub use body::{BodyError, MessageBody};
pub use id::{IdError, MessageId};
pub use shard::{Shard, ShardGroup};

/// Maximum serialized message size — a network-wide wire contract kept
/// under iroh-gossip's payload budget so a message we accept always fits
/// one gossip message (see `crate::util::consts::MAX_MESSAGE_SIZE` for why). Lives
/// in the shared crate; the compile-time assertion below guards the
/// relationship against the live gossip constant.
pub(crate) use crate::util::consts::MAX_MESSAGE_SIZE;

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

/// Protocol version embedded in every message. Bumped to `8.0` for
/// **password-swarm content encryption**: on a passworded swarm the `state`/
/// `meta` change bodies and broadcast chat (`A2aMsg`) bodies now travel as a
/// symmetric `{"k":"enc",…}` envelope instead of cleartext, so a `7.0` peer
/// (which would silently fail to decode an `enc` body) must NOT interoperate.
/// (`7.0` was the automerge `state`/`meta` channels — Base58 change bodies +
/// heads-based digests; `6.0` directed sealing; `5.0` the A2A v1.0 / `ProtoJSON`
/// migration; `4.0` the native-A2A port; `3.0` the A2A migration; `2.0` the RFC
/// 6902 → RFC 7386 shared-state change.) The exact-match gate in `parse` drops
/// cross-version messages loudly rather than letting them silently fold to
/// no-ops and diverge.
pub(crate) const VERSION: &str = "8.0";

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

/// The single addressee of a directed frame — the routing mirror of
/// `gossip::recv::addressed_to_us`. `Some(nick)` for a frame that targets
/// exactly one participant (`Pong`, the A2A task-push legs, the A2A RPC
/// request/response); `None` for a broadcast or infrastructure kind. The
/// [`crate::unicast`] send router uses this to decide point-to-point vs gossip.
///
/// Deliberately **separate** from `addressed_to_us`: that answers a *surfacing*
/// question and treats `Pong` as broadcast-visible, whereas routing wants the
/// `Pong`'s addressee too. Merging them would change the surface/relay filter.
#[must_use]
pub(crate) fn sole_addressee(kind: &MessageKind) -> Option<&Nickname> {
    match kind {
        MessageKind::Pong { to }
        | MessageKind::A2aStatus { to, .. }
        | MessageKind::A2aArtifact { to, .. }
        | MessageKind::A2aReq { to, .. }
        | MessageKind::A2aResp { to, .. } => Some(to),
        MessageKind::A2aMsg
        | MessageKind::Presence { .. }
        | MessageKind::PeerInfo
        | MessageKind::Digest
        | MessageKind::Ping
        | MessageKind::State
        | MessageKind::StateDigest
        | MessageKind::Meta
        | MessageKind::MetaDigest
        | MessageKind::LinkState => None,
    }
}

/// Message kind — the frame's routing discriminator:
/// - `A2aMsg`: **swarm-wide broadcast** chat — the body is a serialized
///   [`crate::a2a::Message`] (declaring the `swarm-broadcast` extension). A2A
///   is point-to-point, so directed 1:1 communication is a **task**
///   (`A2aReq`/`A2aResp` `message/send`), never a directed chat frame.
/// - `Presence`: agent lifecycle (joined/left), empty body.
/// - `PeerInfo`: infrastructure — carries endpoint address for mesh formation. Not user-visible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum MessageKind {
    /// A swarm-wide broadcast A2A chat Message carried whole in `body`
    /// (compact JSON). Broadcast only — it carries no addressee; A2A's
    /// point-to-point traffic rides `A2aReq`/`A2aResp` (task `message/send`).
    #[serde(rename = "a2a_msg")]
    A2aMsg,
    Presence {
        subtype: PresenceSubtype,
    },
    PeerInfo,
    /// Anti-entropy digest. Body is a JSON array of recent message ids
    /// the sender holds; a receiver re-broadcasts any of *its* logged
    /// messages absent from that list, so a peer that missed them
    /// (partition / sleep / late join) recovers. Plumbing like
    /// `PeerInfo`: never logged or surfaced via `poll`/`fetch`.
    Digest,
    /// Liveness probe broadcast by a node running an RTT round. Every
    /// receiver auto-responds with a `Pong` addressed back to the
    /// pinger. Plumbing like `PeerInfo`/`Digest`: never logged or surfaced
    /// via `poll`/`fetch` — only the originator's `ping_report` event surfaces.
    Ping,
    /// Response to a `Ping`, addressed to the original pinger (`to`).
    /// The pinger records its local arrival time to compute RTT. Same
    /// plumbing treatment as `Ping`.
    Pong {
        to: Nickname,
    },
    /// A worker-pushed task status transition or liveness beat, addressed to
    /// the initiator `to` and correlated by `task_id` (duplicated from the
    /// payload so the daemon routes without parsing; receive validates the two
    /// agree). The body is a serialized [`crate::a2a::TaskStatusUpdate`] — the
    /// A2A streaming plane over gossip. Delivered to every peer (gossip floods)
    /// but surfaced and logged only by the addressee and the sender — third
    /// parties relay without retaining. Beats (`metadata:{"swarm:beat":true}`)
    /// are liveness plumbing, never logged. Not part of the per-author hash
    /// chain or DAG (presence-like).
    #[serde(rename = "a2a_status")]
    A2aStatus {
        to: Nickname,
        task_id: crate::a2a::TaskId,
    },
    /// The worker's result: the body is a serialized
    /// [`crate::a2a::TaskArtifactUpdate`]. Receiving it parks the task in
    /// `input-required` for the initiator's approval. Same
    /// addressing/privacy/chain rules as `A2aStatus`.
    #[serde(rename = "a2a_artifact")]
    A2aArtifact {
        to: Nickname,
        task_id: crate::a2a::TaskId,
    },
    /// A directed A2A JSON-RPC **request** to `to`, correlated by `rpc_id`.
    /// The body is a JSON-RPC envelope (`{"method","params"}`); the addressee
    /// serves it as an A2A server (a safe method subset) and replies with an
    /// [`A2aResp`](MessageKind::A2aResp) carrying the same `rpc_id`. Plumbing
    /// like `Ping`/`Pong`: never logged, never chain/DAG-folded, surfaced only
    /// to the addressee for relay — the RPC result reaches the requester via
    /// its parked waiter, never the chat stream.
    #[serde(rename = "a2a_req")]
    A2aReq {
        to: Nickname,
        rpc_id: crate::a2a::A2aRpcId,
    },
    /// The reply to an [`A2aReq`](MessageKind::A2aReq), addressed to the
    /// original requester (`to`) and echoing its `rpc_id`. The body is the
    /// JSON-RPC response (`{"result"}` or `{"error":{code,message}}`). Same
    /// plumbing treatment as `A2aReq`.
    #[serde(rename = "a2a_resp")]
    A2aResp {
        to: Nickname,
        rpc_id: crate::a2a::A2aRpcId,
    },
    /// A durable `state`-channel event: one Base58 automerge change
    /// (`{"k":"change",…}`) applied to the channel's [`SwarmDoc`](crate::daemon::doc)
    /// CRDT. Carried on the gossip topic like everything else; the signed frame
    /// is retained as the doc's re-serve store. Signed like any message; never
    /// entered into the chat message-log, never surfaced via poll/fetch.
    State,
    /// Anti-entropy digest for the **state** channel — the dedicated counterpart
    /// to [`Digest`](MessageKind::Digest). Body is the doc's automerge heads; a
    /// receiver computes the changes the sender lacks (`changes_since`) and
    /// re-broadcasts their frames, so a cold/late joiner (advertising an empty or
    /// genesis-only frontier) pulls the whole history over successive rounds.
    /// Separate from `Digest` for its own resend budget. Plumbing like `Digest`.
    StateDigest,
    /// A durable event on the **meta** channel — a second shared-state channel,
    /// identical machinery to [`State`](MessageKind::State) but with the
    /// foreign-card forgery gate enabled (a `meta` card carries a peer's
    /// cryptographic identity). The split is application convention (`meta` for
    /// swarm metadata, `state` for the task).
    Meta,
    /// Anti-entropy digest for the **meta** channel — the meta-channel counterpart
    /// to [`StateDigest`](MessageKind::StateDigest).
    MetaDigest,
    /// A circuit-routing **link-state** advertisement: the author's own measured
    /// links (`body` is a serialized [`crate::circuit::LinkVector`]). Broadcast
    /// plumbing like `Digest`/`Ping` — never logged, never surfaced, never
    /// chain/DAG-folded; every node folds the freshest vector per origin into its
    /// routing graph. Ephemeral (a fresh vector supersedes the old by `seq`), so
    /// it rides gossip rather than a durable channel log.
    #[serde(rename = "linkstate")]
    LinkState,
}

/// Which shared-state channel an event/digest belongs to. The two channels share
/// all machinery; this selects the wire kind, the log, and the surfaced name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    State,
    Meta,
}

impl Channel {
    pub(crate) fn event_kind(self) -> MessageKind {
        match self {
            Channel::State => MessageKind::State,
            Channel::Meta => MessageKind::Meta,
        }
    }

    pub(crate) fn digest_kind(self) -> MessageKind {
        match self {
            Channel::State => MessageKind::StateDigest,
            Channel::Meta => MessageKind::MetaDigest,
        }
    }

    /// The event name surfaced on the `--output json` stream (`state` / `meta`).
    pub(crate) fn label(self) -> &'static str {
        match self {
            Channel::State => "state",
            Channel::Meta => "meta",
        }
    }
}

impl MessageKind {
    /// Whether a frame of this kind is worth mirroring to the filesystem spool
    /// (`crate::transport::spool`). True for the **durable** content a late or
    /// offline joiner actually needs — chat, task legs, and the shared
    /// `state`/`meta` docs — which is exactly the set anti-entropy re-serves.
    /// **Ephemeral plumbing** is skipped: persisting a `Presence`/`PeerInfo`
    /// frame would resurrect a departed peer when the file is ingested later
    /// (sneakernet, hours on), and digests / ping-pong / link-state / directed
    /// RPC are worthless on disk (they coordinate live peers, not history).
    /// Exhaustive on purpose — a new kind must classify itself here.
    pub(crate) fn is_spoolable(&self) -> bool {
        match self {
            MessageKind::A2aMsg
            | MessageKind::A2aStatus { .. }
            | MessageKind::A2aArtifact { .. }
            | MessageKind::State
            | MessageKind::Meta => true,
            MessageKind::Presence { .. }
            | MessageKind::PeerInfo
            | MessageKind::Digest
            | MessageKind::Ping
            | MessageKind::Pong { .. }
            | MessageKind::A2aReq { .. }
            | MessageKind::A2aResp { .. }
            | MessageKind::StateDigest
            | MessageKind::MetaDigest
            | MessageKind::LinkState => false,
        }
    }
}

impl fmt::Display for MessageKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MessageKind::A2aMsg => write!(f, "a2a_msg"),
            MessageKind::Presence { .. } => write!(f, "presence"),
            MessageKind::PeerInfo => write!(f, "peerinfo"),
            MessageKind::Digest => write!(f, "digest"),
            MessageKind::Ping => write!(f, "ping"),
            MessageKind::Pong { .. } => write!(f, "pong"),
            MessageKind::A2aStatus { .. } => write!(f, "a2a_status"),
            MessageKind::A2aArtifact { .. } => write!(f, "a2a_artifact"),
            MessageKind::A2aReq { .. } => write!(f, "a2a_req"),
            MessageKind::A2aResp { .. } => write!(f, "a2a_resp"),
            MessageKind::State => write!(f, "state"),
            MessageKind::StateDigest => write!(f, "state_digest"),
            MessageKind::Meta => write!(f, "meta"),
            MessageKind::MetaDigest => write!(f, "meta_digest"),
            MessageKind::LinkState => write!(f, "linkstate"),
        }
    }
}

fn default_ext() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

fn empty_body() -> MessageBody {
    MessageBody::new("").expect("empty string is always a valid MessageBody")
}

/// A protocol frame — serialized as JSON on the wire. The transport
/// envelope beneath the A2A layer: for chat (`a2a_msg`), `body` carries a
/// whole serialized [`crate::a2a::Message`]; plumbing kinds (presence,
/// digests, ping/pong, state/meta) carry their own opaque bodies.
///
/// Wire format (compact JSON, one line):
/// ```json
/// {"v":"8.0","id":"<uuid>","type":"a2a_msg","swarm":"💬...","author":"word-word","ts":1234567890,"body":"{\"messageId\":\"<uuid>\",\"role\":\"ROLE_USER\",...}","ext":{}}
/// ```
///
/// `to` (the addressee nickname) is inlined into the JSON for directed `a2a_msg` kinds.
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
    /// Empty on an unsigned message: the empty fields are skipped, which is
    /// also the **canonical pre-signing form** that [`canonical_bytes`] hashes
    /// (the signature cannot cover itself). Real outbound traffic is always
    /// signed on the broadcast path, and the receive path drops anything that
    /// fails verification. See [`docs/history-integrity.md`].
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
    /// Shard header: present only on a message that is one slice of a body
    /// too large for a single message (see [`Shard`]). `None` on an ordinary
    /// message, so its wire form is unchanged. Signed like every other field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard: Option<Shard>,
    /// Extension escape hatch. Add experimental fields here; stable fields get promoted to top-level.
    #[serde(default = "default_ext")]
    pub ext: serde_json::Value,
}

/// Is `value` exactly `bytes * 2` lowercase-hex characters — the canonical
/// wire form of a fixed-width binary field (pubkey / signature / hash)?
fn is_lower_hex(value: &str, bytes: usize) -> bool {
    value.len() == bytes * 2
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
            shard: None,
            ext: default_ext(),
        }
    }

    /// A frame of an explicit `kind` — the generic constructor the task
    /// send path uses to build A2A message/status/artifact frames through
    /// one shard-splitting flow.
    pub(crate) fn new_frame(
        swarm: &SwarmId,
        author: &Nickname,
        kind: MessageKind,
        body: MessageBody,
    ) -> Self {
        Self::new(swarm, author, kind, body)
    }

    /// A swarm-wide broadcast chat frame whose `body` is a serialized
    /// [`crate::a2a::Message`] (declaring the `swarm-broadcast` extension).
    pub(crate) fn new_a2a_msg(swarm: &SwarmId, author: &Nickname, body: MessageBody) -> Self {
        Self::new(swarm, author, MessageKind::A2aMsg, body)
    }

    pub(crate) fn new_joined(swarm: &SwarmId, author: &Nickname) -> Self {
        // Presence carries no body; peer metadata (model/harness/host) is application
        // data an agent writes into the `meta` channel, not a presence payload.
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

    /// A liveness probe (broadcast). Receivers auto-respond with a
    /// `Pong` addressed back to `author`.
    pub(crate) fn new_ping(swarm: &SwarmId, author: &Nickname) -> Self {
        Self::new(swarm, author, MessageKind::Ping, empty_body())
    }

    /// A `Pong` response addressed to the original pinger (`to`).
    pub(crate) fn new_pong(swarm: &SwarmId, author: &Nickname, to: Nickname) -> Self {
        Self::new(swarm, author, MessageKind::Pong { to }, empty_body())
    }

    /// A task status frame addressed to `to`, whose `body` is a serialized
    /// [`crate::a2a::TaskStatusUpdate`] correlated by `task_id`.
    pub(crate) fn new_a2a_status(
        swarm: &SwarmId,
        author: &Nickname,
        to: Nickname,
        task_id: crate::a2a::TaskId,
        body: MessageBody,
    ) -> Self {
        Self::new(swarm, author, MessageKind::A2aStatus { to, task_id }, body)
    }

    /// A directed A2A JSON-RPC request to `to`, whose `body` is a JSON-RPC
    /// `{"method","params"}` envelope correlated by `rpc_id`.
    pub(crate) fn new_a2a_req(
        swarm: &SwarmId,
        author: &Nickname,
        to: Nickname,
        rpc_id: crate::a2a::A2aRpcId,
        body: MessageBody,
    ) -> Self {
        Self::new(swarm, author, MessageKind::A2aReq { to, rpc_id }, body)
    }

    /// The reply to an `A2aReq`, addressed to the requester `to`, whose `body`
    /// is the JSON-RPC response, echoing `rpc_id`.
    pub(crate) fn new_a2a_resp(
        swarm: &SwarmId,
        author: &Nickname,
        to: Nickname,
        rpc_id: crate::a2a::A2aRpcId,
        body: MessageBody,
    ) -> Self {
        Self::new(swarm, author, MessageKind::A2aResp { to, rpc_id }, body)
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

    /// A relay link-state advertisement; `vector_json` is a serialized
    /// [`crate::circuit::LinkVector`].
    pub(crate) fn new_link_state(
        swarm: &SwarmId,
        author: &Nickname,
        vector_json: MessageBody,
    ) -> Self {
        Self::new(swarm, author, MessageKind::LinkState, vector_json)
    }

    /// A durable state event whose opaque payload is `body`. Routed to the
    /// un-pruned `daemon::state_log`, not the chat message-log. Test/harness
    /// constructor; production uses [`new_channel_event`](Self::new_channel_event).
    #[cfg(any(test, feature = "adversarial"))]
    pub(crate) fn new_state(swarm: &SwarmId, author: &Nickname, body: MessageBody) -> Self {
        Self::new(swarm, author, MessageKind::State, body)
    }

    /// A durable event on the given channel (`state` / `meta`).
    pub(crate) fn new_channel_event(
        swarm: &SwarmId,
        author: &Nickname,
        body: MessageBody,
        channel: Channel,
    ) -> Self {
        Self::new(swarm, author, channel.event_kind(), body)
    }

    /// An anti-entropy digest for the given channel.
    pub(crate) fn new_channel_digest(
        swarm: &SwarmId,
        author: &Nickname,
        body: MessageBody,
        channel: Channel,
    ) -> Self {
        Self::new(swarm, author, channel.digest_kind(), body)
    }

    /// A state anti-entropy digest whose `body` is the windowed digest the
    /// sender advertises (a `DigestBody` of `WireWindow`s over the `State`
    /// events it holds — the unbounded analogue of the chat digest). Test-only;
    /// production uses [`new_channel_digest`](Self::new_channel_digest).
    #[cfg(test)]
    pub(crate) fn new_state_digest(swarm: &SwarmId, author: &Nickname, body: MessageBody) -> Self {
        Self::new(swarm, author, MessageKind::StateDigest, body)
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
        // Shape-check the history-integrity fields at the boundary, so a crafted
        // value never reaches signature verification or the fork/DAG indexes
        // (which key on `prev`/`parents` as content hashes). Empty `pubkey`/`sig`
        // is the canonical unsigned form — the receive path drops anything that
        // then fails verification; a *present* key/sig/hash must be well-formed
        // lowercase hex of the right length (Ed25519 pubkey 32B, signature 64B,
        // SHA-256 hash 32B).
        if !msg.pubkey.is_empty() && !is_lower_hex(&msg.pubkey, 32) {
            bail!("malformed pubkey");
        }
        if !msg.sig.is_empty() && !is_lower_hex(&msg.sig, 64) {
            bail!("malformed signature");
        }
        if let Some(prev) = &msg.prev
            && !is_lower_hex(prev, 32)
        {
            bail!("malformed prev hash");
        }
        if msg.parents.iter().any(|hash| !is_lower_hex(hash, 32)) {
            bail!("malformed parent hash");
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
        const DOMAIN: &[u8] = b"agent-gossip/msg";
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
        // The shard header is signed so a shard can't be re-grouped or
        // re-indexed by a relay. `None` (ordinary message) folds as `null`.
        field(&serde_json::to_vec(&self.shard).unwrap_or_default());
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

    /// The 16-byte dedup / anti-entropy key (`SHA-256(pubkey ‖ id)[..16]`).
    /// Dedup, the message/state logs, and the digest all key on this rather
    /// than the sender-chosen id, so a forged message reusing a victim's id
    /// cannot suppress the genuine one. See [`identity::dedup_key16`].
    #[must_use]
    pub(crate) fn dedup_key(&self) -> [u8; 16] {
        identity::dedup_key16(&self.pubkey, &self.id.as_uuid_bytes())
    }

    /// Stamp the per-author log fields before signing (`Msg` only). `seq`
    /// is the author's monotonic counter, `prev` the hash of their previous
    /// `Msg` (`None` at `seq 0`). Consuming-builder so it composes with
    /// [`signed`](Self::signed): `Message::new_a2a_msg(..).with_chain(..).signed(..)`.
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

    /// Stamp the [`Shard`] header (which slice of a split body this
    /// message carries) before signing. `None` leaves it an ordinary message.
    #[must_use]
    pub(crate) fn with_shard(mut self, shard: Option<Shard>) -> Self {
        self.shard = shard;
        self
    }

    /// Replace the minted random id before signing. The chat path pins the
    /// frame id to the payload's A2A `messageId` so every consumer-visible id
    /// (dedup, poll cursor, echo) *is* the A2A id; receive validates the two
    /// agree.
    #[must_use]
    pub(crate) fn with_id(mut self, id: MessageId) -> Self {
        self.id = id;
        self
    }

    /// The serialized wire size **without** the `MAX_MESSAGE_SIZE` gate, for
    /// deciding how to split a body. `serialize` is the gated counterpart.
    #[must_use]
    pub(crate) fn wire_len(&self) -> usize {
        serde_json::to_vec(self).map_or(usize::MAX, |bytes| bytes.len())
    }

    /// Sign this message with `identity`, filling `pubkey` then `sig`.
    /// `pubkey` is set before the canonical bytes are computed so the key
    /// is part of what is signed; consuming-builder style so it composes in
    /// the construction expression (`Message::new_a2a_msg(..).signed(&id)`).
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
    /// Convenience wrapper that computes the canonical bytes itself. The hot
    /// receive path uses [`verify_signature_with`](Self::verify_signature_with)
    /// to share one `canonical_bytes()` with the content hash; this form is
    /// only the ergonomic one used by tests.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn verify_signature(&self) -> bool {
        self.verify_signature_with(&self.canonical_bytes())
    }

    /// Like [`verify_signature`](Self::verify_signature) but against
    /// **precomputed** canonical bytes. The hot receive path computes
    /// `canonical_bytes()` once and reuses it for both this check and the
    /// content hash, instead of re-serializing the message twice per `Msg`.
    /// `canonical` MUST be `self.canonical_bytes()` for the result to be
    /// meaningful.
    #[must_use]
    pub(crate) fn verify_signature_with(&self, canonical: &[u8]) -> bool {
        if self.pubkey.is_empty() || self.sig.is_empty() {
            return false;
        }
        let (Ok(pubkey), Ok(sig)) = (
            identity::decode_pubkey(&self.pubkey),
            identity::decode_sig(&self.sig),
        ) else {
            return false;
        };
        identity::verify(&pubkey, canonical, &sig)
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
/// [`build_msg_bytes`] stays within the argument budget. Test-only now that the
/// send path (`gossip::broadcast`) stamps the chain inline to interleave it with
/// the multipart split.
#[cfg(test)]
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

#[cfg(test)]
pub(crate) fn build_msg_bytes(
    swarm: &SwarmId,
    body: MessageBody,
    author: &Nickname,
    identity: &Identity,
    chain: ChainCtx,
) -> Result<(Bytes, Message)> {
    let msg = Message::new_a2a_msg(swarm, author, body)
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
            version: VERSION.to_string(),
            id: "00000000-0000-0000-0000-000000000001".into(),
            kind,
            swarm: SwarmId::from("💬test"),
            author: "alice-bot".into(),
            timestamp: 1_700_000_000,
            body: body.into(),
            pubkey: String::new(),
            sig: String::new(),
            seq: None,
            prev: None,
            parents: Vec::new(),
            shard: None,
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
        SwarmId::from("💬test")
    }

    #[test]
    fn test_round_trip() {
        let msg = Message::new_a2a_msg(
            &sid(),
            &nick("word-word"),
            MessageBody::from("Hello, world!"),
        );
        let bytes = msg.serialize().unwrap();
        let parsed = Message::parse(&bytes).unwrap();
        assert_eq!(parsed.id, msg.id);
        assert_eq!(parsed.kind, MessageKind::A2aMsg);
        assert_eq!(parsed.body, msg.body);
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
    fn test_task_status_round_trip() {
        let target = nick("calm-otter");
        let task_id = crate::a2a::TaskId::random();
        let msg = Message::new_a2a_status(
            &sid(),
            &nick("word-word"),
            target.clone(),
            task_id.clone(),
            MessageBody::from(r#"{"kind":"status-update"}"#),
        );
        let bytes = msg.serialize().unwrap();
        let wire = String::from_utf8_lossy(&bytes);
        assert!(wire.contains("\"type\":\"a2a_status\""));
        let parsed = Message::parse(&bytes).unwrap();
        assert_eq!(
            parsed.kind,
            MessageKind::A2aStatus {
                to: target,
                task_id,
            }
        );
        assert_eq!(parsed.body, msg.body);
    }

    #[test]
    fn test_task_artifact_round_trip() {
        let target = nick("calm-otter");
        let task_id = crate::a2a::TaskId::random();
        let msg = Message::new_frame(
            &sid(),
            &nick("word-word"),
            MessageKind::A2aArtifact {
                to: target.clone(),
                task_id: task_id.clone(),
            },
            MessageBody::from(r#"{"kind":"artifact-update"}"#),
        );
        let bytes = msg.serialize().unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("\"type\":\"a2a_artifact\""));
        let parsed = Message::parse(&bytes).unwrap();
        assert_eq!(
            parsed.kind,
            MessageKind::A2aArtifact {
                to: target,
                task_id,
            }
        );
    }

    #[test]
    fn test_ext_round_trip() {
        let mut msg =
            Message::new_a2a_msg(&sid(), &nick("word-word"), MessageBody::from("With ext."));
        msg.ext = serde_json::json!({"tags": ["rust", "p2p"], "priority": 1});
        let bytes = msg.serialize().unwrap();
        let parsed = Message::parse(&bytes).unwrap();
        assert_eq!(parsed.ext["tags"][0], "rust");
        assert_eq!(parsed.ext["priority"], 1);
    }

    /// A valid UUID for the hand-written wire-JSON fixtures below (the
    /// validating `MessageId` deserialize rejects non-UUID ids).
    const FIXTURE_ID: &str = "550e8400-e29b-41d4-a716-446655440000";

    #[test]
    fn test_unknown_ext_fields_ignored() {
        let json = format!(
            r#"{{"v":"8.0","id":"{FIXTURE_ID}","type":"a2a_msg","swarm":"💬test","author":"a-b","ts":0,"body":"hi","ext":{{"future_field":"value","another":42}}}}"#
        );
        let parsed = Message::parse(json.as_bytes()).unwrap();
        assert_eq!(parsed.body.as_str(), "hi");
        assert_eq!(parsed.ext["future_field"], "value");
    }

    #[test]
    fn test_missing_ext_defaults_to_empty_object() {
        let json = format!(
            r#"{{"v":"8.0","id":"{FIXTURE_ID}","type":"a2a_msg","swarm":"💬test","author":"a-b","ts":0,"body":"hi"}}"#
        );
        let parsed = Message::parse(json.as_bytes()).unwrap();
        assert_eq!(parsed.ext, serde_json::json!({}));
    }

    #[test]
    fn test_version_mismatch_rejected() {
        // A `1.0` (pre-merge) message must be rejected by this `2.0` binary — the
        // rolling-upgrade guard: cross-version state never silently folds.
        let json = format!(
            r#"{{"v":"1.0","id":"{FIXTURE_ID}","type":"msg","swarm":"💬test","author":"a-b","ts":0,"body":"hi","ext":{{}}}}"#
        );
        assert!(Message::parse(json.as_bytes()).is_err());
    }

    // Crafted wire messages that a correct client never produces: the
    // validating newtype `Deserialize` impls must reject them at `parse`, so a
    // malicious peer cannot crash the daemon (bad id) or inject terminal
    // escapes / spoof the `<nick>`/`#swarm` conventions (bad body/author).
    #[test]
    fn parse_rejects_non_uuid_id() {
        let json = r#"{"v":"8.0","id":"not-a-uuid","type":"a2a_msg","swarm":"💬test","author":"a-b","ts":0,"body":"hi","ext":{}}"#;
        assert!(Message::parse(json.as_bytes()).is_err());
    }

    #[test]
    fn parse_rejects_control_char_body() {
        let json = format!(
            r#"{{"v":"8.0","id":"{FIXTURE_ID}","type":"a2a_msg","swarm":"💬test","author":"a-b","ts":0,"body":"evil\u0000body","ext":{{}}}}"#
        );
        assert!(Message::parse(json.as_bytes()).is_err());
    }

    #[test]
    fn parse_rejects_unsafe_author_nickname() {
        let json = format!(
            r#"{{"v":"8.0","id":"{FIXTURE_ID}","type":"a2a_msg","swarm":"💬test","author":"a#b","ts":0,"body":"hi","ext":{{}}}}"#
        );
        assert!(Message::parse(json.as_bytes()).is_err());
    }

    #[test]
    fn parse_rejects_malformed_integrity_fields() {
        // Each history-integrity field (pubkey / sig / prev / parents) must be
        // rejected at `parse` when present but not well-formed lowercase hex,
        // so a crafted value never reaches the fork/DAG indexes or sig verify.
        let base = |extra: &str| {
            format!(
                r#"{{"v":"8.0","id":"{FIXTURE_ID}","type":"a2a_msg","swarm":"💬test","author":"a-b","ts":0,"body":"hi"{extra},"ext":{{}}}}"#
            )
        };
        // 3KB garbage pubkey, non-hex / wrong-length variants, and a bad hash.
        for extra in [
            format!(r#","pubkey":"{}""#, "z".repeat(3000)),
            r#","pubkey":"AABB""#.to_string(), // uppercase + too short
            r#","sig":"nothex""#.to_string(),
            r#","prev":"xyz""#.to_string(),
            r#","parents":["00"]"#.to_string(), // too short
        ] {
            assert!(
                Message::parse(base(&extra).as_bytes()).is_err(),
                "should reject: {extra}"
            );
        }
        // A well-formed (if unverifiable) 64-hex pubkey still parses — shape
        // only; the signature gate is a separate, later check.
        let ok = base(&format!(r#","pubkey":"{}""#, "ab".repeat(32)));
        assert!(Message::parse(ok.as_bytes()).is_ok());
    }

    #[test]
    fn build_msg_bytes_message() {
        let alice = nick("alice");
        let identity = crate::protocol::identity::Identity::generate();
        let (bytes, built) = build_msg_bytes(
            &sid(),
            MessageBody::from("hello"),
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

    mod signing {
        use super::super::{Message, MessageKind};
        use crate::protocol::identity::Identity;

        fn identity() -> Identity {
            Identity::generate()
        }

        #[test]
        fn signed_message_verifies() {
            let msg = Message::fixture(MessageKind::A2aMsg, "hello").signed(&identity());
            assert!(!msg.pubkey.is_empty() && !msg.sig.is_empty());
            assert!(msg.verify_signature());
        }

        #[test]
        fn unsigned_message_does_not_verify() {
            let msg = Message::fixture(MessageKind::A2aMsg, "hello");
            assert!(!msg.verify_signature(), "empty pubkey/sig must not verify");
        }

        #[test]
        fn tampered_body_breaks_signature() {
            let mut msg = Message::fixture(MessageKind::A2aMsg, "hello").signed(&identity());
            msg.body = "tampered".into();
            assert!(!msg.verify_signature());
        }

        #[test]
        fn tampered_author_breaks_signature() {
            let mut msg = Message::fixture(MessageKind::A2aMsg, "hello").signed(&identity());
            msg.author = "impostor-bot".into();
            assert!(!msg.verify_signature());
        }

        #[test]
        fn tampered_task_target_breaks_signature() {
            let task_id = crate::a2a::TaskId::random();
            let mut msg = Message::fixture(
                MessageKind::A2aStatus {
                    to: "calm-otter".into(),
                    task_id: task_id.clone(),
                },
                "{}",
            )
            .signed(&identity());
            assert!(msg.verify_signature());
            msg.kind = MessageKind::A2aStatus {
                to: "evil-otter".into(),
                task_id,
            };
            assert!(!msg.verify_signature(), "task `to` is a signed field");
        }

        #[test]
        fn tampered_task_id_breaks_signature() {
            let mut msg = Message::fixture(
                MessageKind::A2aStatus {
                    to: "calm-otter".into(),
                    task_id: crate::a2a::TaskId::random(),
                },
                "{}",
            )
            .signed(&identity());
            msg.kind = MessageKind::A2aStatus {
                to: "calm-otter".into(),
                task_id: crate::a2a::TaskId::random(),
            };
            assert!(!msg.verify_signature(), "task `task_id` is a signed field");
        }

        #[test]
        fn signature_survives_wire_round_trip() {
            let msg = Message::fixture(MessageKind::A2aMsg, "hi").signed(&identity());
            let parsed = Message::parse(&msg.serialize().unwrap()).unwrap();
            assert!(parsed.verify_signature());
            assert_eq!(parsed.pubkey, msg.pubkey);
        }

        #[test]
        fn unsigned_wire_omits_signature_fields() {
            // The skip-if-empty fields keep an unsigned message byte-identical
            // to the v1 wire, so existing snapshots are unaffected.
            let bytes = Message::fixture(MessageKind::A2aMsg, "hi")
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
            Message::fixture(MessageKind::A2aMsg, body)
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

        /// A deterministic chat frame carrying a real A2A payload (its random
        /// `messageId` pinned to the fixture id), so the snapshot pins the
        /// payload-in-body wire form, not a placeholder.
        fn chat_fixture(text: &str) -> Message {
            let mut payload =
                crate::a2a::gossip::chat_message(&super::super::SwarmId::from("💬test"), text);
            payload.message_id =
                crate::a2a::MessageId::from("00000000-0000-0000-0000-000000000001");
            let body = crate::a2a::gossip::payload_body(&payload).expect("payload serializes");
            Message::fixture(MessageKind::A2aMsg, body.as_str())
        }

        #[test]
        fn snap_wire_message() {
            let msg = chat_fixture("What is Rust?");
            let bytes = msg.serialize().unwrap();
            let wire = String::from_utf8(bytes).unwrap();
            insta::assert_snapshot!(wire);
        }

        /// The status-frame wire shape: a worker's `working` (accept) status.
        #[test]
        fn snap_wire_task_status() {
            let task_id = crate::a2a::TaskId::from("00000000-0000-0000-0000-000000000001");
            let update = crate::a2a::gossip::status_update(
                &super::super::SwarmId::from("💬test"),
                &task_id,
                crate::a2a::TaskState::Working,
                None,
                None,
            );
            let body = crate::a2a::gossip::payload_body(&update).expect("payload serializes");
            let msg = Message::fixture(
                MessageKind::A2aStatus {
                    to: Nickname::from("addressed-nick"),
                    task_id,
                },
                body.as_str(),
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

        #[test]
        fn snap_wire_state_merge() {
            // A shared-state change rides `MessageKind::State`; its body is the
            // tagged merge envelope (`k:"merge"`) the reducer parses. Pinning the
            // wire bytes guards the discriminator + RFC 7386 merge shape.
            let msg = Message::fixture(MessageKind::State, r#"{"k":"merge","merge":{"turn":"b"}}"#);
            let bytes = msg.serialize().unwrap();
            let wire = String::from_utf8(bytes).unwrap();
            insta::assert_snapshot!(wire);
        }

        /// The gossip A2A **request** frame: directed, correlated by `rpc_id`,
        /// body = a JSON-RPC `{method, params}` envelope.
        #[test]
        fn snap_wire_a2a_req() {
            let msg = Message::fixture(
                MessageKind::A2aReq {
                    to: Nickname::from("addressed-nick"),
                    rpc_id: crate::a2a::A2aRpcId::from("00000000-0000-0000-0000-0000000000aa"),
                },
                r#"{"method":"tasks/list","params":{}}"#,
            );
            let wire = String::from_utf8(msg.serialize().unwrap()).unwrap();
            insta::assert_snapshot!(wire);
        }

        /// The gossip A2A **response** frame, echoing the request's `rpc_id`.
        #[test]
        fn snap_wire_a2a_resp() {
            let msg = Message::fixture(
                MessageKind::A2aResp {
                    to: Nickname::from("addressed-nick"),
                    rpc_id: crate::a2a::A2aRpcId::from("00000000-0000-0000-0000-0000000000aa"),
                },
                r#"{"result":{"tasks":[]}}"#,
            );
            let wire = String::from_utf8(msg.serialize().unwrap()).unwrap();
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
            #![proptest_config(crate::proptest_support::config())]
            #[test]
            fn prop_message_round_trip(
                body in arb_ascii_body(),
                author in arb_nickname(),
            ) {
                let body = MessageBody::new(body).unwrap();
                let msg = Message::new_a2a_msg(&sid(), &author, body);
                let bytes = msg.serialize().unwrap();
                let parsed = Message::parse(&bytes).unwrap();
                prop_assert_eq!(&parsed.body, &msg.body);
                prop_assert_eq!(&parsed.author, &msg.author);
                prop_assert_eq!(&parsed.version, VERSION);
                prop_assert_eq!(parsed.kind, MessageKind::A2aMsg);
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
                let msg = Message::new_a2a_msg(&sid(), &author, body);
                let bytes = msg.serialize().unwrap();
                let parsed = Message::parse(&bytes).unwrap();
                prop_assert_eq!(&parsed.body, &expected);
            }

            #[test]
            fn prop_serialized_size_within_limit(
                body in arb_ascii_body(),
            ) {
                let msg = Message::new_a2a_msg(
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
