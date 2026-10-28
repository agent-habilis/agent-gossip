use std::time::Duration;

use habilis_network::protocol::Nickname;

use crate::a2a::TaskId;

/// The call describing a directed [`MeshSession::a2a_call`](super::MeshSession::a2a_call) /
/// `InProcessSession::a2a_call` — which peer, which JSON-RPC method, its
/// params, and how long to wait for the reply.

#[derive(Debug)]
#[non_exhaustive]
pub struct A2aCallParams {
    /// The peer to call.
    pub peer: Nickname,
    /// The JSON-RPC method name (`PascalCase` per the safe A2A subset).
    pub method: String,
    /// The method's JSON-RPC params.
    pub params: serde_json::Value,
    /// How long to wait for the peer's reply before giving up.
    pub timeout: Duration,
}

/// The result artifact for [`MeshSession::task_artifact`](super::MeshSession::task_artifact): the text plus an
/// optional file, split into its blob-offload parts (`name`/`mime` describe
/// `file`, so they only mean anything when it is `Some`).

#[derive(Debug)]
#[non_exhaustive]
pub struct TaskArtifactParams {
    /// The task this artifact belongs to.
    pub task_id: TaskId,
    /// The artifact's text part.
    pub text: String,
    /// An optional file to offload over the blob channel and reference by
    /// `Part.url`.
    pub file: Option<std::path::PathBuf>,
    /// `file`'s display name, if any.
    pub file_name: Option<String>,
    /// `file`'s MIME type, if any.
    pub file_mime: Option<String>,
}

/// The status leg for [`MeshSession::task_status`](super::MeshSession::task_status):
/// the new state, plus an optional note and an optional reason that the
/// worker waits on its own user (only meaningful with `working`).
#[derive(Debug)]
#[non_exhaustive]
pub struct TaskStatusParams {
    /// The task this status belongs to.
    pub task_id: TaskId,
    /// The new state.
    pub state: crate::a2a::TaskState,
    /// The status message text, if any.
    pub note: Option<String>,
    /// Why the worker waits on its own user, if it does.
    pub waiting: Option<String>,
}

impl A2aCallParams {
    #[must_use]
    pub fn new(
        peer: Nickname,
        method: String,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Self {
        Self {
            peer,
            method,
            params,
            timeout,
        }
    }
}

impl TaskArtifactParams {
    /// An artifact that carries only text.
    #[must_use]
    pub fn new(task_id: TaskId, text: String) -> Self {
        Self {
            task_id,
            text,
            file: None,
            file_name: None,
            file_mime: None,
        }
    }

    /// Attach a file to offload over the blob channel.
    #[must_use]
    pub fn with_file(mut self, file: std::path::PathBuf) -> Self {
        self.file = Some(file);
        self
    }
}

impl TaskStatusParams {
    /// A status with no note and no waiting reason.
    #[must_use]
    pub fn new(task_id: TaskId, state: crate::a2a::TaskState) -> Self {
        Self {
            task_id,
            state,
            note: None,
            waiting: None,
        }
    }

    #[must_use]
    pub fn with_note(mut self, note: String) -> Self {
        self.note = Some(note);
        self
    }

    /// The worker waits on its own user for this reason.
    #[must_use]
    pub fn with_waiting(mut self, reason: String) -> Self {
        self.waiting = Some(reason);
        self
    }
}
