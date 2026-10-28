use habilis_network::embed::RosterSnapshot;
use habilis_network::protocol::message::Authored;
use habilis_network::protocol::{MeshId, MeshName, MessageBody, Nickname};
use habilis_network::runtime::{EventLoopConfig, Node};
use tokio::sync::{broadcast, oneshot};

use super::a2a_call_params::{A2aCallParams, TaskStatusParams};
use super::advertiser::Advertiser;
use super::create_error::{CreateError, JoinError};
use super::join_config::{CreateConfig, JoinConfig, TopicConfig};
use super::session_error::SessionError;
use super::spawn_env::{SpawnEnv, create_setup, join_setup, topic_setup};
use crate::a2a::TaskId;
use crate::a2a::a2a_app::{A2aApp, SurfacedIo};
use crate::a2a::broadcast_params::FileRef;
use crate::a2a::session_request::SessionRequest;
use crate::a2a::surfaced_event::PollBatch;
use crate::output::Output;

/// The in-process session core shared by the public [`MeshSession`] and the MCP
/// server. Wraps the engine's [`Node`] with this app's request/reply vocabulary
/// and the optional directory advertiser. The two frontends differ only in
/// *presentation*: `MeshSession` adds the inbound broadcast + captured-event
/// stream; the MCP server wraps this core directly (poll-only, silent output).

#[derive(Debug)]
pub(crate) struct InProcessSession {
    node: Node<A2aApp>,
    /// Directory re-broadcast task (when created with `advertise`). Aborts on
    /// drop — see [`Advertiser`].
    advertiser: Option<Advertiser>,
}

impl InProcessSession {
    /// Spawn the event loop and build the core. `push` is `Some` to fan inbound
    /// traffic out to a broadcast ([`MeshSession::messages`]), `None` for a
    /// poll-only consumer (MCP). `env.handle_signals`: see
    /// [`DriverMode::InProcess`] — `false` for sessions living inside a
    /// foreground command that owns its own lifetime.
    pub(super) fn spawn(
        elc: EventLoopConfig,
        io: SurfacedIo,
        env: SpawnEnv,
        push: Option<broadcast::Sender<Authored>>,
    ) -> Self {
        let SpawnEnv {
            advertiser,
            handle_signals,
        } = env;
        // The tapped `io` (built in `*_setup`) already backs `elc.sink`: the
        // engine emits `NodeEvent`s through `io.sink()`, and the app renders the
        // same tap. Build the app from that same `io` so surfaced events reach
        // the app-side ring the in-process `Poll` drains.
        let app = A2aApp::new_with_io(io);
        Self {
            node: Node::spawn(elc, app, push, handle_signals),
            advertiser,
        }
    }

    /// As [`Self::call`], for a loop answer that is itself an `anyhow` result.
    async fn call_loop<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<anyhow::Result<T>>) -> SessionRequest,
    ) -> Result<T, SessionError> {
        self.call(build).await?.map_err(SessionError::Loop)
    }

    /// Push a request carrying a `oneshot` reply channel and await the answer.
    /// `build` receives the reply sender and returns the variant to send, so
    /// both failure legs — the loop stopped, the loop dropped the channel —
    /// collapse here instead of being restated at each request method below.
    async fn call<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<T>) -> SessionRequest,
    ) -> Result<T, SessionError> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.node
            .send(build(resp_tx))
            .await
            .map_err(|_| SessionError::Stopped)?;
        resp_rx.await.map_err(|_| SessionError::DroppedResponse)
    }

    /// Create a new mesh as a poll-only, silent core (the MCP server).
    ///
    /// # Errors
    /// [`CreateError::AdvertiseRequiresReachable`] / [`CreateError::Setup`].
    pub(crate) async fn create_poll(config: CreateConfig) -> Result<Self, CreateError> {
        let (elc, io, advertiser) = create_setup(config, Output::silent()).await?;
        Ok(Self::spawn(
            elc,
            io,
            SpawnEnv {
                advertiser,
                handle_signals: true,
            },
            None,
        ))
    }

    /// Join an existing mesh as a poll-only, silent core (the MCP server).
    ///
    /// # Errors
    /// [`JoinError::Resolve`] / [`JoinError::Setup`].
    pub(crate) async fn join_poll(config: JoinConfig) -> Result<Self, JoinError> {
        let (elc, io) = join_setup(config, Output::silent()).await?;
        Ok(Self::spawn(
            elc,
            io,
            SpawnEnv {
                advertiser: None,
                handle_signals: true,
            },
            None,
        ))
    }

    /// Join a topic (string-derived public mesh) as a poll-only, silent core
    /// (the MCP server).
    ///
    /// # Errors
    /// [`JoinError::Resolve`] on an empty string; [`JoinError::Setup`] on
    /// endpoint/gossip failure.
    pub(crate) async fn topic_poll(config: TopicConfig) -> Result<Self, JoinError> {
        let (elc, io) = topic_setup(config, Output::silent()).await?;
        Ok(Self::spawn(
            elc,
            io,
            SpawnEnv {
                advertiser: None,
                handle_signals: true,
            },
            None,
        ))
    }

    pub(crate) fn mesh_id(&self) -> &MeshId {
        self.node.mesh_id()
    }

    pub(crate) fn name(&self) -> &MeshName {
        self.node.name()
    }

    pub(crate) fn nickname(&self) -> &Nickname {
        self.node.nickname()
    }

    /// Build, sign and gossip-broadcast a message; returns the canonical
    /// [`Message`] the loop built, or `None` when the sender-side rate
    /// limiter dropped it.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub(crate) async fn broadcast(&self, body: MessageBody) -> Result<Authored, SessionError> {
        // Two Results: the outer `?` is the channel, the inner is the loop's answer.
        self.call_loop(|response| SessionRequest::Broadcast { body, response })
            .await
    }

    /// Send a chat message to one peer; returns the canonical [`Message`] the
    /// loop built.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response, or if the
    /// peer's seal key has not replicated yet (a msg is never sent in plaintext).
    pub(crate) async fn message(
        &self,
        to: Nickname,
        body: MessageBody,
    ) -> Result<Authored, SessionError> {
        self.call_loop(|response| SessionRequest::Msg { to, body, response })
            .await
    }

    /// Poll the surfaced-event history after the `after` seq cursor
    /// (join-horizon filtered). Returns the seq-tagged surfaced events — the
    /// same events the live stream shows, ready to render via
    /// [`crate::output::surfaced_event_json`].
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub(crate) async fn fetch(
        &self,
        after: Option<u64>,
        long: bool,
    ) -> Result<PollBatch, SessionError> {
        self.call(|response| SessionRequest::Poll {
            after,
            long,
            response,
        })
        .await
    }

    /// Worker-emit a `TaskStatusUpdate` on a task we're serving; returns the
    /// canonical [`Message`].
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub(crate) async fn task_status(
        &self,
        status: TaskStatusParams,
    ) -> Result<Authored, SessionError> {
        let TaskStatusParams {
            task_id,
            state,
            note,
            waiting,
        } = status;
        self.call_loop(|response| SessionRequest::TaskStatus {
            task_id,
            state,
            note,
            waiting,
            response,
        })
        .await
    }

    /// Worker-emit a `TaskArtifactUpdate` (the result) on a task we're serving.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub(crate) async fn task_artifact(
        &self,
        task_id: TaskId,
        text: String,
        file: Option<FileRef>,
    ) -> Result<Authored, SessionError> {
        self.call_loop(|response| SessionRequest::TaskArtifact {
            task_id,
            text,
            file,
            response,
        })
        .await
    }

    /// Call a peer's A2A server over gossip (request/response) and return the
    /// parsed JSON-RPC response object (`{"result"|"error"}`). Blocks until
    /// the peer answers or `timeout` elapses.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub(crate) async fn a2a_call(
        &self,
        call: A2aCallParams,
    ) -> Result<serde_json::Value, SessionError> {
        let A2aCallParams {
            peer,
            method,
            params,
            timeout,
        } = call;
        self.call(|response| SessionRequest::A2aCall {
            peer,
            method,
            params,
            timeout,
            response,
        })
        .await
    }

    /// Snapshot the live peer roster (active + quiet, recency-sorted).
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub(crate) async fn peers(&self) -> Result<RosterSnapshot, SessionError> {
        self.call(|response| SessionRequest::Peers { response })
            .await
    }

    /// Run an RTT round and return the per-peer round-trip rows. Blocks for
    /// the ping window (the round finalizes on its deadline), so a quiet mesh
    /// returns an empty list after that delay.
    pub(crate) async fn ping(&self) -> Result<Vec<crate::output::PingPeer>, SessionError> {
        let rows = self
            .call(|response| SessionRequest::Ping { response })
            .await?;
        // Map the engine's chat-agnostic RTT rows onto the app's public ping
        // datum — same fields, distinct layers.
        Ok(rows
            .into_iter()
            .map(|(nickname, rtt_ms)| crate::output::PingPeer { nickname, rtt_ms })
            .collect())
    }

    /// Apply an RFC 7386 JSON Merge Patch to the shared state. Any JSON value is a
    /// valid merge; `Err` is a transport/serialize failure only.
    pub(crate) async fn state_merge(&self, merge: serde_json::Value) -> Result<(), SessionError> {
        self.call_loop(|response| SessionRequest::StateMerge { merge, response })
            .await
    }

    /// The current derived shared-state document (the merge fold).
    pub(crate) async fn state_get(&self) -> Result<serde_json::Value, SessionError> {
        self.call(|response| SessionRequest::StateGet { response })
            .await
    }

    /// `meta`-channel counterpart of [`state_merge`](Self::state_merge).
    pub(crate) async fn meta_merge(&self, merge: serde_json::Value) -> Result<(), SessionError> {
        self.call_loop(|response| SessionRequest::MetaMerge { merge, response })
            .await
    }

    /// `meta`-channel counterpart of [`state_get`](Self::state_get).
    pub(crate) async fn meta_get(&self) -> Result<serde_json::Value, SessionError> {
        self.call(|response| SessionRequest::MetaGet { response })
            .await
    }

    /// Broadcast pre-built wire bytes verbatim (no signing/chain). Testkit
    /// only — the injection point for crafted/malicious messages.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    #[cfg(feature = "adversarial")]
    pub(crate) async fn inject_raw(&self, bytes: bytes::Bytes) -> Result<(), SessionError> {
        self.call_loop(|response| SessionRequest::InjectRaw { bytes, response })
            .await
    }

    /// Simulate the gossip stream terminally ending. Adversarial-suite
    /// only — drives the stream-end resubscribe recovery test.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    #[cfg(feature = "adversarial")]
    pub(crate) async fn sever_gossip(&self) -> Result<(), SessionError> {
        self.node
            .send(SessionRequest::SeverGossip)
            .await
            .map_err(|_| SessionError::Stopped)
    }

    /// Snapshot the reassembly store's accounting
    /// `(groups, total_bytes, max_author_bytes)`. Adversarial-suite only.
    #[cfg(feature = "adversarial")]
    pub(crate) async fn reassembly_stats(&self) -> Result<(usize, usize, usize), SessionError> {
        self.call(|response| SessionRequest::ReassemblyStats { response })
            .await
    }

    /// Clean shutdown: ask the loop to broadcast `Left` and wind down,
    /// waiting up to 3s. On timeout returns `Ok(())` and `Drop` detaches.
    ///
    /// # Errors
    /// Returns an error if the event-loop task panicked or returned an error.
    pub(crate) async fn leave(self) -> Result<(), SessionError> {
        let Self {
            node, advertiser, ..
        } = self;
        // Stop advertising first — we're leaving, so the listing should age out
        // rather than keep being re-broadcast while the loop winds down.
        drop(advertiser);
        node.leave().await.map_err(SessionError::LeaveFailed)
    }
}
