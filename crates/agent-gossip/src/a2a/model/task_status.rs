use serde::{Deserialize, Serialize};

use super::{Message, TaskState};

/// A task state plus the optional message that explains the move into it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct TaskStatus {
    /// The state the task is in.
    pub state: TaskState,
    /// A note for the move into `state`, such as a question or a result summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Box<Message>>,
    /// RFC 3339 timestamp — the envelope's clock stays authoritative for
    /// ordering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}
