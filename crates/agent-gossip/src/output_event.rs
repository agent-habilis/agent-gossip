use std::sync::Arc;

use habilis_network::protocol::messages::Authored;
use habilis_network::protocol::{AuthorKey, MeshId, MeshName, Nickname, UnixSecs};

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
        /// `true` when the progress of this check-in is in another quarter than the last one
        /// the record held, which wakes the sender so its row can show the move.
        quarter_moved: bool,
    },
    /// A task leg that arrived over the request/response binding, so no frame backs it.
    TaskMessage {
        /// The id of the A2A message.
        id: String,
        /// When the node surfaced the leg. A later poll prints the same time as the stream.
        ts: UnixSecs,
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
        /// `Part.url`). `None` when the leg has no message to show.
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
    /// The worker's agent has not checked in for 2·X. States the fact only; the
    /// task is not canceled.
    TaskOverdue {
        /// What the alarm says.
        notice: OverdueNotice,
    },
    /// This worker owes a check-in. Rings its bell only; it is never shown.
    CheckinDue {
        /// What the reminder says.
        notice: CheckinNotice,
    },
    /// The worker told this sender, by a beat, of a leg that never arrived, and the task that the
    /// worker then reported is waiting on the sender's answer. Rings its bell only; it is never
    /// shown.
    TaskGap {
        /// What the event says.
        notice: GapNotice,
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
    /// The worker's beat numbered its legs backwards: it started the task over, so this node
    /// ended it.
    WorkerLost,
}

/// The alarm a sender raises when a worker's agent has not checked in for 2·X. The fields are
/// the keys of the `task_overdue` line.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[non_exhaustive]
pub struct OverdueNotice {
    /// The overdue task.
    pub task_id: TaskId,
    /// The worker whose agent has not checked in.
    pub peer: Nickname,
    /// The initiator's name for the task, when the brief carried one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Whole seconds since the last check-in.
    pub age_secs: u64,
    /// Whether the worker said in its last check-in that it waits on its own user.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub waiting_user: bool,
    /// `true` when the worker never accepted the task.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub not_picked_up: bool,
}

/// The event of a question that was lost on the way.
///
/// The friendly name of a task state, the one the agent surface speaks.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's `serialize_with` passes the field by reference"
)]
fn friendly_state<S: serde::Serializer>(state: &TaskState, out: S) -> Result<S::Ok, S::Error> {
    out.serialize_str(state.friendly_name())
}

/// The beat of the worker showed a leg that never arrived, and the task that the worker then
/// reported waits for the answer of the sender, or is over. The fields are the keys of the
/// `task_gap` line.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[non_exhaustive]
pub struct GapNotice {
    /// The task that waits for an answer.
    pub task_id: TaskId,
    /// The worker.
    pub peer: Nickname,
    /// The state that the worker reported: `input-required`, or an end.
    #[serde(serialize_with = "friendly_state")]
    pub state: TaskState,
    /// What the worker said last, which is the question when the worker asked one. Absent when the
    /// task waits on a result to review, because the last words are then an older check-in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// `true` when the cap cut `text`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// `true` when the task waits on a result to review, which the lost leg carried.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub review: bool,
}

/// The reminder a worker raises when it owes a check-in. The fields are the keys of the
/// `checkin_due` line.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[non_exhaustive]
pub struct CheckinNotice {
    /// The task that waits for a check-in.
    pub task_id: TaskId,
    /// The initiator.
    pub peer: Nickname,
    /// The brief or follow-up the worker has not answered yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// `true` when the cap cut `text`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// `true` when that message carries a file the reminder does not repeat.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub has_file: bool,
    /// `true` when that message is the opening brief, so a worker that lost its todo can tell a
    /// brief from a follow-up.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub brief: bool,
}
