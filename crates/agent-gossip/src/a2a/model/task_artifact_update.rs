use serde::{Deserialize, Serialize};

use super::{Artifact, TaskId};

/// A streamed event that adds to, or replaces, one artifact of a task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct TaskArtifactUpdate {
    /// The task that owns the artifact.
    pub task_id: TaskId,
    /// The mesh id the task lives in.
    pub context_id: String,
    /// The artifact, or the new chunk of it.
    pub artifact: Artifact,
    /// `Some(true)` adds the parts to the stored artifact; otherwise they replace it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub append: Option<bool>,
    /// `Some(true)` marks the final chunk of a chunked artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_chunk: Option<bool>,
    /// Free-form JSON that rides along with the update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}
