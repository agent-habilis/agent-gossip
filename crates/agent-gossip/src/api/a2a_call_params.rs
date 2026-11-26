use std::time::Duration;

use habilis_network::protocol::Nickname;

use crate::a2a::TaskId;
use crate::a2a::status_update_params::Progress;

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
/// It holds the new state, plus a note, a progress and an optional tag naming
/// whom the worker waits on. A `working` status is a check-in and needs the note
/// and the progress; no other state takes a progress or a tag.
#[derive(Debug)]
#[non_exhaustive]
pub struct TaskStatusParams {
    /// The task this status belongs to.
    pub task_id: TaskId,
    /// The new state.
    pub state: crate::a2a::TaskState,
    /// The status message text. Required on `working`, where it says what changed.
    pub note: Option<String>,
    /// How far the work is. Required on `working`, refused on any other state.
    pub progress: Option<Progress>,
    /// Whom the worker waits on (a peer or `user`), if it does.
    pub waiting_on: Option<String>,
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
    /// A status with no note, no progress and no waiting tag.
    #[must_use]
    pub fn new(task_id: TaskId, state: crate::a2a::TaskState) -> Self {
        Self {
            task_id,
            state,
            note: None,
            progress: None,
            waiting_on: None,
        }
    }

    /// Put `note` on the status message.
    #[must_use]
    pub fn with_note(mut self, note: String) -> Self {
        self.note = Some(note);
        self
    }

    /// Report how far the work is.
    #[must_use]
    pub fn with_progress(mut self, progress: Progress) -> Self {
        self.progress = Some(progress);
        self
    }

    /// The worker waits on `who` (a peer or `user`).
    #[must_use]
    pub fn with_waiting_on(mut self, who: String) -> Self {
        self.waiting_on = Some(who);
        self
    }
}
