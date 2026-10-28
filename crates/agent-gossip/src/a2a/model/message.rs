use serde::{Deserialize, Serialize};

use super::{MessageId, Part, Role, TaskId};

/// One A2A v1.0 message: a sender-minted id, a role, and one or more parts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Message {
    /// Minted by the sender; a receiver uses it to deduplicate.
    pub message_id: MessageId,
    /// Who wrote the message: the requester (`User`) or the worker (`Agent`).
    pub role: Role,
    /// The content, in order.
    pub parts: Vec<Part>,
    /// The mesh id that groups related messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    /// The task this message belongs to; `None` for a message that starts none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    /// Other tasks this message points at without joining them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reference_task_ids: Vec<TaskId>,
    /// URIs of the extensions that apply to this message, such as `EXT_MESH_BROADCAST`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    /// Free-form JSON, for example the `mesh:*` keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl Message {
    /// A minimal message with a fresh id and one text part; callers stamp
    /// `context_id` / `task_id` / `extensions` as the operation requires.
    #[must_use]
    pub fn text(role: Role, text: &str) -> Self {
        Message {
            message_id: MessageId::random(),
            role,
            parts: vec![Part::text(text)],
            context_id: None,
            task_id: None,
            reference_task_ids: Vec::new(),
            extensions: Vec::new(),
            metadata: None,
        }
    }
}
