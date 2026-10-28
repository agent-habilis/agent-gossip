use serde::{Deserialize, Serialize};

use super::{Artifact, Message, TaskId, TaskStatus};

/// A unit of delegated work and its current status, outputs, and history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Task {
    /// Minted by the server that creates the task.
    pub id: TaskId,
    /// The mesh id the task lives in.
    pub context_id: String,
    /// The latest status.
    pub status: TaskStatus,
    /// The outputs produced so far.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<Artifact>,
    /// The messages exchanged on this task, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<Message>,
    /// Free-form JSON that rides along with the task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}
