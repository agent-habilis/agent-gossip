use std::time::Duration;

use habilis_network::protocol::Nickname;

use crate::a2a::{TaskId, WorkerLeg};

/// The call that a directed [`MeshSession::a2a_call`](super::MeshSession::a2a_call) makes.
///
/// It names which peer, which JSON-RPC method, its params, and how long to wait for the reply.
/// `InProcessSession::a2a_call` takes it too.

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

/// The result artifact for [`MeshSession::task_artifact`](super::MeshSession::task_artifact).
///
/// It holds the text plus an
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

/// The status leg for [`MeshSession::task_status`](super::MeshSession::task_status).
///
/// The [`WorkerLeg`] holds the state, the note, the progress and the flag that the worker waits
/// on its user, already checked: a `working` status is a check-in and needs the note and the
/// progress, and no other state takes a progress or the flag.
#[derive(Debug)]
#[non_exhaustive]
pub struct TaskStatusParams {
    /// The task this status belongs to.
    pub task_id: TaskId,
    /// The leg to send.
    pub leg: WorkerLeg,
}

impl A2aCallParams {
    /// A call to `peer` for `method`, with the reply deadline `timeout`.
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
    /// The status `leg` for the task `task_id`.
    #[must_use]
    pub fn new(task_id: TaskId, leg: WorkerLeg) -> Self {
        Self { task_id, leg }
    }
}
