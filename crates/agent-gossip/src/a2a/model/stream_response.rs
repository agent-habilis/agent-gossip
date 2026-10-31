use serde::{Deserialize, Serialize};

use super::{Message, Task, TaskArtifactUpdate, TaskStatusUpdate};

/// A2A v1.0 `StreamResponse`: the SSE / gossip-stream payload oneof. Each
/// streamed event is one of these, ProtoJSON-tagged by member name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum StreamResponse {
    /// A full snapshot of the task.
    Task(Task),
    /// A message on the task.
    Message(Message),
    /// A change of task status.
    StatusUpdate(TaskStatusUpdate),
    /// A new or changed artifact.
    ArtifactUpdate(TaskArtifactUpdate),
}
