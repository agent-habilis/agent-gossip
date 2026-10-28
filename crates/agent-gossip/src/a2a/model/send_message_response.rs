use serde::{Deserialize, Serialize};

use super::{Message, Task};

/// A2A v1.0 `SendMessageResponse`: a proto oneof — `ProtoJSON` serializes the set
/// member by name, so the JSON-RPC `result` is `{"task":…}` or `{"message":…}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum SendMessageResponse {
    Task(Task),
    Message(Message),
}
