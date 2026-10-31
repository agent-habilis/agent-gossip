use serde::{Deserialize, Serialize};

use super::{TaskId, TaskStatus};

/// A streamed event that moves a task to a new status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct TaskStatusUpdate {
    /// The task that moves.
    pub task_id: TaskId,
    /// The mesh id the task lives in.
    pub context_id: String,
    /// The new status.
    pub status: TaskStatus,
    /// Free-form JSON, for example the beat keys from `beat_metadata`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}
