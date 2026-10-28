use serde::{Deserialize, Serialize};

use super::{Message, TaskState};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct TaskStatus {
    pub state: TaskState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Box<Message>>,
    /// RFC 3339 timestamp — the envelope's clock stays authoritative for
    /// ordering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}
