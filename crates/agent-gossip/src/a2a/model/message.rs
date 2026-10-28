use serde::{Deserialize, Serialize};

use super::{MessageId, Part, Role, TaskId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Message {
    pub message_id: MessageId,
    pub role: Role,
    pub parts: Vec<Part>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reference_task_ids: Vec<TaskId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
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
