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
mod part;
mod task_id;

pub use body::{BodyError, MessageBody};
pub use id::{IdError, MessageId};
pub use part::{Part, PartGroup};
pub use task_id::{TaskId, TaskIdError};

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

/// Protocol version embedded in every message. Bumped to `2.0` for the
/// RFC 6902 → RFC 7386 shared-state wire-contract change: the `State`/`Meta`
/// body shape changed incompatibly (`{"k":"patch","ops":[…]}` → `{"k":"merge",
/// "merge":{…}}`), so a `1.0` peer and a `2.0` peer must NOT interoperate. The
/// exact-match gate in `parse` drops cross-version messages loudly rather than
/// letting them silently fold to no-ops and diverge the shared document.
pub(crate) const VERSION: &str = "2.0";

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

/// Phase of a task — the behavior-agnostic lifecycle
/// every task shares. `Offer` opens with the brief; `Accept`/
/// `Decline` are the entry decision; `Context` carries the bidirectional
/// Q&A; `Progress` is the receiver's liveness+percent heartbeat (plumbing,
/// like `Alive`); `Done` requests close; `Confirm`/`Change` are the
/// initiator's verify decision (`Change` loops back to `Context`); `Cancel`
/// aborts. See the daemon task state machine for the transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskPhase {
    Offer,
    Accept,
    Decline,
    Context,
    Progress,
    Done,
    Confirm,
    Change,
    Cancel,
}

impl fmt::Display for TaskPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskPhase::Offer => write!(f, "offer"),
            TaskPhase::Accept => write!(f, "accept"),
            TaskPhase::Decline => write!(f, "decline"),
            TaskPhase::Context => write!(f, "context"),
            TaskPhase::Progress => write!(f, "progress"),
            TaskPhase::Done => write!(f, "done"),
            TaskPhase::Confirm => write!(f, "confirm"),
            TaskPhase::Change => write!(f, "change"),
            TaskPhase::Cancel => write!(f, "cancel"),
        }
    }
}

/// Error parsing a [`TaskPhase`] from its lowercase string form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPhaseError(String);

impl fmt::Display for TaskPhaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid phase '{}' (expected offer|accept|decline|context|progress|done|confirm|change|cancel)",
            self.0
        )
    }
}

impl std::error::Error for TaskPhaseError {}

/// Parse a phase from its lowercase string. The CLI `--phase` parser and the
/// MCP `send_task` tool both delegate here, so the accepted set is defined
/// once (and [`TaskPhase`]'s `Display` is its inverse).
impl std::str::FromStr for TaskPhase {
    type Err = TaskPhaseError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw {
            "offer" => Ok(TaskPhase::Offer),
            "accept" => Ok(TaskPhase::Accept),
            "decline" => Ok(TaskPhase::Decline),
            "context" => Ok(TaskPhase::Context),
            "progress" => Ok(TaskPhase::Progress),
            "done" => Ok(TaskPhase::Done),
            "confirm" => Ok(TaskPhase::Confirm),
            "change" => Ok(TaskPhase::Change),
            "cancel" => Ok(TaskPhase::Cancel),
            other => Err(TaskPhaseError(other.to_owned())),
        }
    }
}

/// Is this phase a **content** leg (counts toward the per-task message
/// cap, logged like `Msg`)? `Progress` is the only non-content task
/// phase — it is liveness plumbing (exempt from the cap, never logged), the
/// rest carry real conversation.
#[must_use]
pub(crate) fn is_content_phase(phase: TaskPhase) -> bool {
    !matches!(phase, TaskPhase::Progress)
}

/// The single addressee of a directed message — the routing mirror of
/// `gossip::recv::addressed_to_us`. `Some(nick)` for a message that targets
/// exactly one participant (a directed `Msg`/`Notice`, any `Task` leg, a
/// `Pong`); `None` for a broadcast or infrastructure kind. The [`crate::unicast`]
/// send router uses this to decide point-to-point vs gossip.
///
/// Deliberately **separate** from `addressed_to_us`: that answers a *surfacing*
/// question and treats `Pong` as broadcast-visible, whereas routing wants the
/// `Pong`'s addressee too. Merging them would change the embed-push filter.
#[must_use]
pub(crate) fn sole_addressee(kind: &MessageKind) -> Option<&Nickname> {
    match kind {
        MessageKind::Msg { reply: Some(to) }
        | MessageKind::Notice { reply: Some(to) }
        | MessageKind::Task { to, .. }
        | MessageKind::Pong { to } => Some(to),
        MessageKind::Msg { reply: None }
        | MessageKind::Notice { reply: None }
        | MessageKind::Presence { .. }
        | MessageKind::PeerInfo
        | MessageKind::Digest
        | MessageKind::Ping
        | MessageKind::State
        | MessageKind::StateDigest
        | MessageKind::Meta
        | MessageKind::MetaDigest => None,
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
    /// A no-auto-reply message (IRC NOTICE semantics): identical to `Msg` on
    /// every path — directed via `reply`, chained, fork-detected, logged,
    /// multipart-splittable — except for the receiver contract that an agent
    /// must NEVER auto-respond to one. The distinct kind is what makes the
    /// contract enforceable by construction: status broadcasts, CI results
    /// and log lines sent as notices can never start an agent reply loop.
    Notice {
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
    /// One leg of a task, addressed to `to` and correlated by
    /// `task_id` (so both sides group the legs into one conversation).
    /// `phase` is the lifecycle position. Delivered to every peer (gossip
    /// floods) but surfaced and logged only by the addressee and the sender —
    /// third parties relay without retaining, exactly like a directed `Msg`.
    /// **Content** phases are logged with `Msg`; the `Progress` phase is
    /// liveness plumbing (never logged). Not part of the per-author hash chain
    /// or DAG (presence-like). How the two parties *use* a task (delegate a
    /// plan, run work and return a result, …) is a skill-land convention
    /// carried in the offer body — the primitive itself has no notion of it.
    Task {
        to: Nickname,
        task_id: TaskId,
        phase: TaskPhase,
    },
    /// A durable swarm-state event (membership edits, settings, …). Carried
    /// on the same gossip topic as everything else but routed to a **separate,
    /// un-pruned** log (`daemon::state_log`); swarm state is the deterministic
    /// fold over that log. The payload lives opaquely in `body` — the log layer
    /// never interprets it; projections (a future allowlist, …) do. Signed like
    /// any message; never entered into the chat message-log, never surfaced
    /// via poll/fetch.
    State,
    /// Anti-entropy digest for the **state** log — the dedicated counterpart to
    /// [`Digest`](MessageKind::Digest). Body is the Base58-packed ids of the
    /// `State` events the sender holds; a receiver re-broadcasts any state event
    /// absent from that set, so a cold/late joiner (advertising an empty set)
    /// pulls the whole state log. Separate from `Digest` for its own
    /// gossip-message and resend budgets. Plumbing like `Digest`.
    StateDigest,
    /// A durable event on the **meta** channel — a second shared-state channel,
    /// byte-for-byte identical to [`State`](MessageKind::State) in every respect
    /// (own un-pruned log, own anti-entropy, own derived doc). The binary does
    /// not differentiate the two; the split is application convention (`meta` for
    /// swarm metadata, `state` for the task).
    Meta,
    /// Anti-entropy digest for the **meta** log — the meta-channel counterpart to
    /// [`StateDigest`](MessageKind::StateDigest).
    MetaDigest,
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

impl fmt::Display for MessageKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MessageKind::Msg { .. } => write!(f, "msg"),
            MessageKind::Notice { .. } => write!(f, "notice"),
            MessageKind::Presence { .. } => write!(f, "presence"),
            MessageKind::PeerInfo => write!(f, "peerinfo"),
            MessageKind::Digest => write!(f, "digest"),
            MessageKind::Ping => write!(f, "ping"),
            MessageKind::Pong { .. } => write!(f, "pong"),
            MessageKind::Task { .. } => write!(f, "task"),
            MessageKind::State => write!(f, "state"),
            MessageKind::StateDigest => write!(f, "state_digest"),
            MessageKind::Meta => write!(f, "meta"),
            MessageKind::MetaDigest => write!(f, "meta_digest"),
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
/// {"v":"2.0","id":"<uuid>","type":"msg","swarm":"💬...","author":"word-word","ts":1234567890,"body":"text","ext":{}}
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
    /// Multipart header: present only on a message that is one slice of a body
    /// too large for a single message (see [`Part`]). `None` on an ordinary
    /// message, so its wire form is unchanged. Signed like every other field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<Part>,
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
            part: None,
            ext: default_ext(),
        }
    }

    /// An open `Msg`. Test/harness convenience; production chat construction
    /// goes through [`new_chat`](Self::new_chat), which the parameterized
    /// send path uses for `Msg` and `Notice` alike.
    #[cfg(any(test, feature = "adversarial", feature = "bench"))]
    pub(crate) fn new_message(swarm: &SwarmId, author: &Nickname, body: MessageBody) -> Self {
        Self::new(swarm, author, MessageKind::Msg { reply: None }, body)
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

    /// A directed `Msg`. Test-only sibling of [`new_message`](Self::new_message).
    #[cfg(test)]
    pub(crate) fn new_reply(
        swarm: &SwarmId,
        author: &Nickname,
        reply: Nickname,
        body: MessageBody,
    ) -> Self {
        Self::new(swarm, author, MessageKind::Msg { reply: Some(reply) }, body)
    }

    /// A chat message of a caller-chosen kind — `Msg` or `Notice`, open or
    /// directed. The one construction point the parameterized send path uses
    /// so both kinds share it verbatim; the non-chat kinds have their own
    /// constructors and never come through here.
    pub(crate) fn new_chat(
        swarm: &SwarmId,
        author: &Nickname,
        kind: MessageKind,
        body: MessageBody,
    ) -> Self {
        debug_assert!(
            matches!(kind, MessageKind::Msg { .. } | MessageKind::Notice { .. }),
            "new_chat takes only the chat kinds"
        );
        Self::new(swarm, author, kind, body)
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

    /// One leg of a task addressed to `to`, correlated by
    /// `task_id`. `Offer` carries the task brief in the body; `Context`
    /// the Q&A; `Progress` a `done/total` fraction; `Done`/`Change`/
    /// `Decline`/`Cancel` an optional summary/reason; `Accept`/`Confirm`
    /// an optional note.
    pub(crate) fn new_task(
        swarm: &SwarmId,
        author: &Nickname,
        to: Nickname,
        task_id: TaskId,
        phase: TaskPhase,
        body: MessageBody,
    ) -> Self {
        Self::new(
            swarm,
            author,
            MessageKind::Task { to, task_id, phase },
            body,
        )
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
        // The multipart header is signed so a part can't be re-grouped or
        // re-indexed by a relay. `None` (ordinary message) folds as `null`.
        field(&serde_json::to_vec(&self.part).unwrap_or_default());
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

    /// Stamp the multipart [`Part`] header (which slice of a split body this
    /// message carries) before signing. `None` leaves it an ordinary message.
    #[must_use]
    pub(crate) fn with_part(mut self, part: Option<Part>) -> Self {
        self.part = part;
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
            part: None,
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
    fn test_notice_round_trip() {
        let msg = Message::new_chat(
            &sid(),
            &nick("word-word"),
            MessageKind::Notice { reply: None },
            MessageBody::from("build green"),
        );
        let bytes = msg.serialize().unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("\"type\":\"notice\""));
        let parsed = Message::parse(&bytes).unwrap();
        assert_eq!(parsed.kind, MessageKind::Notice { reply: None });
        assert_eq!(parsed.body.as_str(), "build green");
    }

    #[test]
    fn test_directed_notice_round_trip() {
        let target = nick("calm-otter");
        let msg = Message::new_chat(
            &sid(),
            &nick("word-word"),
            MessageKind::Notice {
                reply: Some(target.clone()),
            },
            MessageBody::from("heads up"),
        );
        let parsed = Message::parse(&msg.serialize().unwrap()).unwrap();
        assert_eq!(
            parsed.kind,
            MessageKind::Notice {
                reply: Some(target)
            }
        );
    }

    #[test]
    fn signed_notice_verifies_and_covers_the_kind() {
        let identity = crate::protocol::identity::Identity::generate();
        let msg = Message::fixture(MessageKind::Notice { reply: None }, "hello").signed(&identity);
        assert!(msg.verify_signature());
        // Flipping the kind to `Msg` must break the signature: the kind is
        // part of the canonical bytes, so a relay cannot demote a notice
        // into an auto-replyable msg.
        let mut forged = msg;
        forged.kind = MessageKind::Msg { reply: None };
        assert!(!forged.verify_signature());
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
    fn test_task_round_trip() {
        use super::{TaskId, TaskPhase};
        let target = nick("calm-otter");
        let task_id = TaskId::random();
        let msg = Message::new_task(
            &sid(),
            &nick("word-word"),
            target.clone(),
            task_id.clone(),
            TaskPhase::Offer,
            MessageBody::from("## Task\nport the parser"),
        );
        let bytes = msg.serialize().unwrap();
        let wire = String::from_utf8_lossy(&bytes);
        assert!(wire.contains("\"type\":\"task\""));
        assert!(wire.contains("\"phase\":\"offer\""));
        let parsed = Message::parse(&bytes).unwrap();
        assert_eq!(
            parsed.kind,
            MessageKind::Task {
                to: target,
                task_id,
                phase: TaskPhase::Offer,
            }
        );
        assert_eq!(parsed.body, msg.body);
    }

    #[test]
    fn task_phase_from_str_round_trips_display() {
        use super::TaskPhase;
        for phase in [
            TaskPhase::Offer,
            TaskPhase::Accept,
            TaskPhase::Decline,
            TaskPhase::Context,
            TaskPhase::Progress,
            TaskPhase::Done,
            TaskPhase::Confirm,
            TaskPhase::Change,
            TaskPhase::Cancel,
        ] {
            let rendered = phase.to_string();
            assert_eq!(rendered.parse::<TaskPhase>().unwrap(), phase);
        }
        assert!("bogus".parse::<TaskPhase>().is_err());
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

    /// A valid UUID for the hand-written wire-JSON fixtures below (the
    /// validating `MessageId` deserialize rejects non-UUID ids).
    const FIXTURE_ID: &str = "550e8400-e29b-41d4-a716-446655440000";

    #[test]
    fn test_unknown_ext_fields_ignored() {
        let json = format!(
            r#"{{"v":"2.0","id":"{FIXTURE_ID}","type":"msg","swarm":"💬test","author":"a-b","ts":0,"body":"hi","ext":{{"future_field":"value","another":42}}}}"#
        );
        let parsed = Message::parse(json.as_bytes()).unwrap();
        assert_eq!(parsed.body.as_str(), "hi");
        assert_eq!(parsed.ext["future_field"], "value");
    }

    #[test]
    fn test_missing_ext_defaults_to_empty_object() {
        let json = format!(
            r#"{{"v":"2.0","id":"{FIXTURE_ID}","type":"msg","swarm":"💬test","author":"a-b","ts":0,"body":"hi"}}"#
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
        let json = r#"{"v":"2.0","id":"not-a-uuid","type":"msg","swarm":"💬test","author":"a-b","ts":0,"body":"hi","ext":{}}"#;
        assert!(Message::parse(json.as_bytes()).is_err());
    }

    #[test]
    fn parse_rejects_control_char_body() {
        let json = format!(
            r#"{{"v":"2.0","id":"{FIXTURE_ID}","type":"msg","swarm":"💬test","author":"a-b","ts":0,"body":"evil\u0000body","ext":{{}}}}"#
        );
        assert!(Message::parse(json.as_bytes()).is_err());
    }

    #[test]
    fn parse_rejects_unsafe_author_nickname() {
        let json = format!(
            r#"{{"v":"2.0","id":"{FIXTURE_ID}","type":"msg","swarm":"💬test","author":"a#b","ts":0,"body":"hi","ext":{{}}}}"#
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
                r#"{{"v":"2.0","id":"{FIXTURE_ID}","type":"msg","swarm":"💬test","author":"a-b","ts":0,"body":"hi"{extra},"ext":{{}}}}"#
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
            MessageKind::Notice { .. }
            | MessageKind::Presence { .. }
            | MessageKind::PeerInfo
            | MessageKind::Digest
            | MessageKind::StateDigest
            | MessageKind::MetaDigest
            | MessageKind::Ping
            | MessageKind::Pong { .. }
            | MessageKind::State
            | MessageKind::Meta
            | MessageKind::Task { .. } => {
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
        fn tampered_task_target_breaks_signature() {
            use super::super::{TaskId, TaskPhase};
            let task_id = TaskId::random();
            let mut msg = Message::fixture(
                MessageKind::Task {
                    to: "calm-otter".into(),
                    task_id: task_id.clone(),
                    phase: TaskPhase::Offer,
                },
                "brief",
            )
            .signed(&identity());
            assert!(msg.verify_signature());
            msg.kind = MessageKind::Task {
                to: "evil-otter".into(),
                task_id,
                phase: TaskPhase::Offer,
            };
            assert!(!msg.verify_signature(), "task `to` is a signed field");
        }

        #[test]
        fn tampered_task_phase_breaks_signature() {
            use super::super::{TaskId, TaskPhase};
            let task_id = TaskId::random();
            let mut msg = Message::fixture(
                MessageKind::Task {
                    to: "calm-otter".into(),
                    task_id: task_id.clone(),
                    phase: TaskPhase::Offer,
                },
                "brief",
            )
            .signed(&identity());
            msg.kind = MessageKind::Task {
                to: "calm-otter".into(),
                task_id,
                phase: TaskPhase::Confirm,
            };
            assert!(!msg.verify_signature(), "task `phase` is a signed field");
        }

        #[test]
        fn tampered_task_id_breaks_signature() {
            use super::super::{TaskId, TaskPhase};
            let mut msg = Message::fixture(
                MessageKind::Task {
                    to: "calm-otter".into(),
                    task_id: TaskId::random(),
                    phase: TaskPhase::Offer,
                },
                "brief",
            )
            .signed(&identity());
            msg.kind = MessageKind::Task {
                to: "calm-otter".into(),
                task_id: TaskId::random(),
                phase: TaskPhase::Offer,
            };
            assert!(!msg.verify_signature(), "task `task_id` is a signed field");
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
        fn snap_wire_task_offer() {
            let msg = Message::fixture(
                MessageKind::Task {
                    to: Nickname::from("addressed-nick"),
                    task_id: super::super::TaskId::from("550e8400-e29b-41d4-a716-446655440000"),
                    phase: super::super::TaskPhase::Offer,
                },
                "## Task\nport the parser",
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
