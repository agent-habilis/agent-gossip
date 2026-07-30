use anyhow::Result;
use tokio::sync::oneshot;

use crate::a2a::TaskId;
use agent_habilis_mesh::embed::RosterSnapshot;
use agent_habilis_mesh::protocol::{Message, MessageBody, Nickname};

/// A typed in-process request from an in-process session to the event
/// loop — the shared alternative to the CLI's `IpcCommand`-over-socket
/// (which must serialize). `Send` broadcasts a message and echoes back the
/// canonical [`Message`]; `Poll` reads the buffered history after a cursor.
pub(crate) enum SessionRequest {
    /// Broadcast a mesh chat message; echoes back the canonical [`Message`].
    Send {
        body: MessageBody,
        resp: oneshot::Sender<Result<Message>>,
    },
    Poll {
        after: Option<u64>,
        /// Long-poll: park the read up to the server cap, returning early on
        /// the first new event. `false` is an immediate read.
        long: bool,
        resp: oneshot::Sender<crate::a2a::surfaced::PollBatch>,
    },
    /// Worker-emit a `TaskStatusUpdate` on a task we're serving (`a2a status`):
    /// the daemon resolves the peer from the task record and pushes it.
    TaskStatus {
        task_id: TaskId,
        state: crate::a2a::TaskState,
        note: Option<String>,
        resp: oneshot::Sender<Result<Message>>,
    },
    /// Worker-emit a `TaskArtifactUpdate` (the result) on a task we're serving
    /// (`a2a artifact`).
    TaskArtifact {
        task_id: TaskId,
        text: String,
        file: Option<crate::a2a::send::FileRef>,
        resp: oneshot::Sender<Result<Message>>,
    },
    /// Snapshot the live peer roster (active + quiet, recency-sorted).
    Peers {
        resp: oneshot::Sender<RosterSnapshot>,
    },
    /// Run an RTT round: broadcast a ping, collect pongs for the ping window,
    /// then deliver the per-peer RTT rows. The response arrives only when the
    /// round finalizes (after the window), not immediately.
    Ping {
        resp: oneshot::Sender<Vec<(Nickname, u64)>>,
    },
    /// Apply an RFC 7386 JSON Merge Patch to the shared state: compose the body,
    /// then sign + gossip. `Err` carries a transport/serialize failure only — a
    /// merge always applies.
    StateMerge {
        merge: serde_json::Value,
        resp: oneshot::Sender<Result<()>>,
    },
    /// Read the current derived shared-state document (the merge fold).
    StateGet {
        resp: oneshot::Sender<serde_json::Value>,
    },
    /// `meta`-channel counterpart of [`StateMerge`](SessionRequest::StateMerge).
    MetaMerge {
        merge: serde_json::Value,
        resp: oneshot::Sender<Result<()>>,
    },
    /// `meta`-channel counterpart of [`StateGet`](SessionRequest::StateGet).
    MetaGet {
        resp: oneshot::Sender<serde_json::Value>,
    },
    /// A **gossip A2A call**: send a directed A2A JSON-RPC request to `peer`
    /// (which serves the safe method set) and await its response. The reply
    /// arrives only when the peer answers or `timeout` elapses — the response
    /// is the parsed JSON-RPC object (`{"result"|"error"}`).
    A2aCall {
        peer: Nickname,
        method: String,
        params: serde_json::Value,
        timeout: std::time::Duration,
        resp: oneshot::Sender<serde_json::Value>,
    },
    /// Broadcast pre-built wire bytes **verbatim** — no signing, no chain
    /// stamping. The escape hatch the `adversarial` feature uses to inject
    /// crafted/malicious messages (bad signature, equivocation, backdating)
    /// that a correct client would never produce, so the adversarial test
    /// suite can prove receivers reject/flag them. Reachable only through
    /// the adversarial-gated `MeshSession::inject_raw`.
    #[cfg(feature = "adversarial")]
    InjectRaw { bytes: bytes::Bytes },
    /// Simulate the gossip event stream terminally ending (flips
    /// `gossip_open` off, exactly what the real `None` arm does — the
    /// orphaned receiver is dropped when the heal arm resubscribes).
    /// Adversarial-suite only: drives the stream-end recovery test
    /// without needing to kill the gossip actor from outside.
    #[cfg(feature = "adversarial")]
    SeverGossip,
    /// Snapshot the reassembly store's accounting
    /// `(groups, total_bytes, max_author_bytes)`. Adversarial-suite only.
    #[cfg(feature = "adversarial")]
    ReassemblyStats {
        resp: oneshot::Sender<(usize, usize, usize)>,
    },
}
