use std::sync::Arc;

use habilis_network::protocol::messages::Authored;
use habilis_network::protocol::{AuthorKey, MeshId, MeshName, MessageId, Nickname};

use crate::a2a::{TaskId, TaskState};

/// One thing a daemon surfaced to its operator or agent. Each variant maps to
/// one `{"event": ...}` line of the `--output json` stream.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum OutputEvent {
    /// Another member with another key uses our nickname: the two nodes do not accept each other's
    /// frames, and both get this event. The stable key is `nickname_held`.
    NicknameHeld {
        /// The nickname that another member holds.
        nickname: String,
    },
    /// The daemon joined the mesh and is ready. First event of a session.
    Ready {
        /// The mesh the daemon joined.
        mesh: MeshId,
        /// The human-readable mesh name.
        name: MeshName,
        /// The nickname of this node in the mesh.
        nickname: Nickname,
        /// Warning text when the installed skills are stale; `None` when current.
        drift: Option<String>,
        /// The bound `--a2a-serve` port; `None` when that binding is off.
        a2a_port: Option<u16>,
    },
    /// A chat message from a peer, or an echo of the local node's own.
    Message {
        /// The signed message as it came off the mesh.
        message: Box<Authored>,
        /// `true` when the local node wrote it.
        is_self: bool,
        /// The addressee's name, read from the roster when the line printed. `None` for a
        /// broadcast, or when the addressee is not live.
        to_name: Option<Nickname>,
    },
    /// A presence frame (a peer's join, leave, or other status change).
    Presence {
        /// The signed presence frame.
        message: Box<Authored>,
        /// The key of the member that the presence is about. A synthetic presence is signed by
        /// this node, so the key of `message` is not the key of the peer.
        peer: AuthorKey,
    },
    /// A peer went silent past the liveness window.
    PeerTimeout {
        /// The silent peer.
        nickname: Nickname,
        /// The author key of the silent peer.
        key: AuthorKey,
        /// Whole seconds since the peer was last heard.
        last_seen_secs_ago: u64,
    },
    /// A peer that timed out was heard again.
    PeerReturn {
        /// The returning peer.
        nickname: Nickname,
        /// The author key of the returning peer.
        key: AuthorKey,
    },
    /// An author signed two different messages at the same `seq`: proof of a fork.
    Fork {
        /// The offending author.
        nickname: Nickname,
        /// The author's full public key, hex; the identity behind the nickname.
        pubkey: String,
        /// The sequence number that carries two messages.
        seq: u64,
    },
    /// The local node posted a message to the mesh.
    MsgPosted {
        /// The id of the posted message.
        id: MessageId,
    },
    /// A notice for the operator. Not an error.
    Info {
        /// The notice text.
        message: String,
    },
    /// Surfaced events that never reached the poll history because the channel that feeds it was
    /// full. It takes the place of the lost events in the ring, so a reader sees where they were.
    EventsLost {
        /// How many events were dropped.
        lost: u64,
    },
    /// An error the operator must see. The daemon keeps running.
    Error {
        /// The error text.
        message: String,
    },
    /// The result of a ping round.
    PingReport {
        /// The peers that answered, with their round-trip times.
        peers: Vec<PingPeer>,
        /// How many peers the node knew about when it pinged.
        known: usize,
    },
    /// A task frame from the gossip binding.
    Task {
        /// The signed frame that carries the task leg.
        message: Box<Authored>,
        /// `true` when the local node sent it.
        is_self: bool,
        /// The addressee's name, read from the roster when the line printed. `None` when the
        /// addressee is not live.
        to_name: Option<Nickname>,
    },
    /// A task leg that arrived over the request/response binding, so no frame backs it.
    TaskMessage {
        /// The id of the A2A message.
        id: String,
        /// The id of the mesh.
        mesh: String,
        /// The nickname of the sender of the leg.
        author: String,
        /// The key of the sender of the leg: a nickname is a label that two keys can share.
        author_key: AuthorKey,
        /// The nickname of the peer the leg goes to (`to` in the JSON line).
        peer: String,
        /// The id of the task the leg belongs to.
        task_id: String,
        /// The state of the task when the leg arrived; `None` when the node tracks no task yet.
        state: Option<TaskState>,
        /// The text parts of the message, joined.
        text: String,
        /// The initiator's `mesh:label` for the task, when the brief carried
        /// one — absent from a peer that does not set it.
        label: Option<String>,
        /// The A2A `Message` the leg carried, for its non-text parts (a file's
        /// `Part.url`). `None` on the initiator's adopted-`Task` leg.
        message: Option<Box<crate::a2a::Message>>,
        /// `true` when the local node sent the leg.
        is_self: bool,
    },
    /// A task left the live set without a terminal status from a peer.
    TaskTimeout {
        /// The task that left.
        task_id: TaskId,
        /// Which trigger removed it.
        reason: TaskGoneReason,
    },
    /// The worker's agent has not polled for `check_secs` and holds no bell.
    /// States the fact only; the task is not canceled.
    TaskStalled {
        /// The stalled task.
        task_id: TaskId,
        /// The worker whose agent is not reading.
        peer: Nickname,
        /// Whole seconds since the agent last polled.
        last_poll_secs_ago: u64,
        /// The silence threshold, in seconds, that triggered this event.
        check_secs: u64,
    },
    /// A delivery or read receipt for a message the local node sent.
    Receipt {
        /// The peer that sent the receipt.
        from: Nickname,
        /// The id of the message the receipt answers (`ref` in the JSON line).
        ref_id: String,
        /// Either `delivered` or `read`.
        state: String,
        /// When the receipt was sent, in Unix seconds.
        ts: u64,
    },
    /// A shared-state document changed.
    StateChanged {
        /// The state channel that changed.
        channel: habilis_network::protocol::Channel,
        /// The signed event that caused the change.
        event: Box<Authored>,
        /// The whole document after the change, shared by the copies of this event. `None` in the
        /// copy that feeds the surfaced-events ring when `channel` is `Meta`: the poll JSON drops
        /// the Meta document, and the ring holds up to `SURFACED_EVENTS_CAP` events, so a Meta
        /// document per event would be the agent card of every peer, many times over.
        document: Option<Arc<serde_json::Value>>,
        /// `true` when the local node made the change.
        is_self: bool,
    },
}

/// One peer's answer in a [`OutputEvent::PingReport`].
#[derive(Debug, Clone, serde::Serialize)]
#[non_exhaustive]
pub struct PingPeer {
    /// The peer that answered.
    pub nickname: Nickname,
    /// Round-trip time in milliseconds.
    pub rtt_ms: u64,
}

/// Why a task left the live set without a peer-authored terminal status:
/// the idle-debounce sweep, or its counterparty's graceful `Left`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum TaskGoneReason {
    /// The idle sweep found no activity on the task.
    Timeout,
    /// The counterparty left the mesh gracefully.
    PeerLeft,
}
