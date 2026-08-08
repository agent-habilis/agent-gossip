pub(crate) mod app;
pub(crate) mod card;
mod extensions;
pub mod gossip;
pub(crate) mod gossip_rpc;
pub(crate) mod http;
pub(crate) mod ipc;
mod model;
pub(crate) mod node;
pub(crate) mod rpc;
pub(crate) mod send;
pub(crate) mod session;
pub(crate) mod surfaced;
pub(crate) mod task;
pub(crate) mod tuning;
pub(crate) mod wire;

pub use extensions::{
    EXT_MESH_A2A_RPC, EXT_MESH_BLOB, EXT_MESH_BROADCAST, EXT_MESH_SEAL, EXT_MESH_STATE,
    GOSSIP_BINDING, META_BEAT, META_DONE, META_LABEL, META_REASON, META_TOTAL,
};
pub use model::{
    A2aRpcId, AgentCapabilities, AgentCard, AgentExtension, AgentInterface, AgentSkill, Artifact,
    Message, MessageId, PROTOCOL_VERSION, Part, Role, SendMessageResponse, StreamResponse, Task,
    TaskArtifactUpdate, TaskId, TaskState, TaskStatus, TaskStatusUpdate, friendly_state,
};

// Compatibility facade: the HTTP tunnel is a bridge subsystem, not A2A core.
pub(crate) use crate::bridge::{
    ExposeParams, TicketDirectory, TicketDirectoryEvent, connect, expose, ticket_requires_password,
};

/// The `meta` channel's per-peer write gate for the A2A data model: each peer
/// owns `peers.<nick>.card` and no one else may write it.
///
/// A function, not a `const`, only because [`SelfWriteGate`] holds `String`s.
/// One home so the pair cannot drift between the four setup paths that install
/// it — the engine needs it to plant the genesis entry and to refuse a forgery.
///
/// [`SelfWriteGate`]: fofoca::embed::SelfWriteGate
pub(crate) fn card_gate() -> fofoca::embed::SelfWriteGate {
    fofoca::embed::SelfWriteGate {
        map: "peers".to_owned(),
        field: "card".to_owned(),
    }
}
