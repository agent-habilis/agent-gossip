use crate::a2a::{TaskId, TaskState};
use agent_habilis_mesh::protocol::mesh::MeshName;
use agent_habilis_mesh::protocol::{MeshId, Message, MessageId, Nickname};

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum OutputEvent {
    Ready { mesh: MeshId, name: MeshName, nickname: Nickname, drift: Option<String>, a2a_port: Option<u16> },
    MeshId { id: MeshId },
    Message { msg: Box<Message>, is_self: bool },
    Presence { msg: Box<Message> },
    PeerTimeout { nickname: Nickname, last_seen_secs_ago: u64 },
    PeerReturn { nickname: Nickname },
    Fork { nickname: Nickname, pubkey: String, seq: u64 },
    MsgPosted { id: MessageId },
    Info { message: String },
    Error { message: String },
    PingReport { peers: Vec<PingPeer>, known: usize },
    Task { msg: Box<Message>, is_self: bool },
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
    TaskTimeout { task_id: TaskId },
    StateChanged {
        channel: agent_habilis_mesh::protocol::Channel,
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
