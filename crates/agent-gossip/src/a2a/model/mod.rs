pub use self::agent_capabilities::AgentCapabilities;
pub use self::agent_card::AgentCard;
pub use self::agent_extension::AgentExtension;
pub use self::agent_interface::AgentInterface;
pub use self::agent_skill::AgentSkill;
pub use self::artifact::Artifact;
pub use self::message::Message;
pub use self::part::Part;
pub use self::protocol_version::PROTOCOL_VERSION;
pub use self::role::Role;
pub use self::send_message_response::SendMessageResponse;
pub use self::stream_response::StreamResponse;
pub use self::task::Task;
pub use self::task_artifact_update::TaskArtifactUpdate;
pub use self::task_state::TaskState;
pub use self::task_status::TaskStatus;
pub use self::task_status_update::TaskStatusUpdate;
pub use self::uuid_id::{A2aRpcId, MessageId, TaskId};

/// A serde `with` module that (de)serializes a [`TaskState`] by its friendly kebab name.
///
/// It serves our internal surfaces (the IPC socket, MCP tool args),
/// keeping the `ProtoJSON` `TASK_STATE_*` form confined to the A2A wire objects.
pub mod friendly_state;

mod agent_capabilities;
mod agent_card;
mod agent_extension;
mod agent_interface;
mod agent_skill;
mod artifact;
mod message;
mod part;
mod protocol_version;
mod role;
mod send_message_response;
mod stream_response;
mod task;
mod task_artifact_update;
mod task_state;
mod task_status;
mod task_status_update;
#[cfg(test)]
mod tests;
mod uuid_id;
