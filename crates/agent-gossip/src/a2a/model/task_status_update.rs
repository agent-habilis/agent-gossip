use serde::{Deserialize, Serialize};

use super::{TaskId, TaskStatus};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct TaskStatusUpdate {
    pub task_id: TaskId,
    pub context_id: String,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}
