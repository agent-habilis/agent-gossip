use crate::a2a::{TaskId, TaskState};
use fofoca::protocol::MeshName;
use fofoca::protocol::{MeshId, Message, MessageId, Nickname};

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
        msg: Box<Message>,
        is_self: bool,
    },
    Presence {
        msg: Box<Message>,
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
        msg: Box<Message>,
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
        is_self: bool,
    },
    TaskTimeout {
        task_id: TaskId,
        reason: TaskGoneReason,
    },
    StateChanged {
        channel: fofoca::protocol::Channel,
        event: Box<Message>,
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
