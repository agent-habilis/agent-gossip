use habilis_network::protocol::message::Authored;
use habilis_network::protocol::{MeshId, MeshName, MessageId, Nickname};

use crate::a2a::{TaskId, TaskState};

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum OutputEvent {
    Ready {
        mesh: MeshId,
        name: MeshName,
        nickname: Nickname,
        drift: Option<String>,
        a2a_port: Option<u16>,
    },
    Message {
        message: Box<Authored>,
        is_self: bool,
    },
    Presence {
        message: Box<Authored>,
    },
    PeerTimeout {
        nickname: Nickname,
        last_seen_secs_ago: u64,
    },
    PeerReturn {
        nickname: Nickname,
    },
    Fork {
        nickname: Nickname,
        pubkey: String,
        seq: u64,
    },
    MsgPosted {
        id: MessageId,
    },
    Info {
        message: String,
    },
    Error {
        message: String,
    },
    PingReport {
        peers: Vec<PingPeer>,
        known: usize,
    },
    Task {
        message: Box<Authored>,
        is_self: bool,
    },
    TaskMessage {
        id: String,
        mesh: String,
        author: String,
        peer: String,
        task_id: String,
        state: Option<TaskState>,
        text: String,
        /// The initiator's `mesh:label` for the task, when the brief carried
        /// one — absent from a peer that does not set it.
        label: Option<String>,
        /// The A2A `Message` the leg carried, for its non-text parts (a file's
        /// `Part.url`). `None` on the initiator's adopted-`Task` leg.
        message: Option<Box<crate::a2a::Message>>,
        is_self: bool,
    },
    TaskTimeout {
        task_id: TaskId,
        reason: TaskGoneReason,
    },
    /// The worker's agent has not polled for `check_secs` and holds no bell.
    /// States the fact only; the task is not canceled.
    TaskStalled {
        task_id: TaskId,
        peer: Nickname,
        last_poll_secs_ago: u64,
        check_secs: u64,
    },
    Receipt {
        from: Nickname,
        ref_id: String,
        state: String,
        ts: u64,
    },
    StateChanged {
        channel: habilis_network::protocol::Channel,
        event: Box<Authored>,
        document: serde_json::Value,
        is_self: bool,
    },
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PingPeer {
    pub nickname: Nickname,
    pub rtt_ms: u64,
}

/// Why a task left the live set without a peer-authored terminal status:
/// the idle-debounce sweep, or its counterparty's graceful `Left`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskGoneReason {
    Timeout,
    PeerLeft,
}
