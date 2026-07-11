use tokio::sync::{broadcast, oneshot};

use super::advertise::Advertiser;
use super::config::{CreateConfig, JoinConfig, TopicConfig};
use super::error::{CreateError, JoinError};
use super::params::A2aCallParams;
use super::setup::{SpawnEnv, create_setup, join_setup, topic_setup};
use crate::a2a::TaskId;
use crate::a2a::session::SessionRequest;
use crate::output::Output;
use agent_habilis_mesh::daemon::state::RosterSnapshot;
use agent_habilis_mesh::daemon::{EventLoopConfig, Node};
use agent_habilis_mesh::protocol::mesh::MeshName;
use agent_habilis_mesh::protocol::{MeshId, Message, MessageBody, Nickname};

/// The in-process session core shared by the public [`MeshSession`] and the MCP
/// server. Wraps the engine's [`Node`] with this app's request/reply vocabulary
/// and the optional directory advertiser. The two frontends differ only in
/// *presentation*: `MeshSession` adds the inbound broadcast + captured-event
/// stream; the MCP server wraps this core directly (poll-only, silent output).

#[derive(Debug)]
pub(crate) struct InProcessSession {
    node: Node<crate::a2a::app::A2aApp>,
    /// This node's iroh endpoint id + X25519 circuit key, captured for the
    /// testkit `inject_link_vector` (which needs a peer's id + key to build the
    /// synthetic link-state vector). `Node` does not carry them.
    #[cfg(feature = "adversarial")]
    endpoint_id: iroh::EndpointId,
    #[cfg(feature = "adversarial")]
    circuit_key: [u8; 32],
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
        io: crate::a2a::app::SurfacedIo,
        env: SpawnEnv,
        push: Option<broadcast::Sender<Message>>,
    ) -> Self {
        let SpawnEnv {
            advertiser,
            handle_signals,
        } = env;
        // Read these before `Node::spawn` consumes `elc`.
        #[cfg(feature = "adversarial")]
        let endpoint_id = elc.endpoint.id();
        #[cfg(feature = "adversarial")]
        let circuit_key = elc.identity.seal_public();
        // The tapped `io` (built in `*_setup`) already backs `elc.sink`: the
        // engine emits `NodeEvent`s through `io.sink()`, and the app renders the
        // same tap. Build the app from that same `io` so surfaced events reach
        // the app-side ring the in-process `Poll` drains.
        let app = crate::a2a::app::A2aApp::with_io(io);
        Self {
            node: Node::spawn(elc, app, push, handle_signals),
            #[cfg(feature = "adversarial")]
            endpoint_id,
            #[cfg(feature = "adversarial")]
            circuit_key,
            advertiser,
        }
    }

    /// Push a request carrying a `oneshot` reply channel and await the answer.
    /// `build` receives the reply sender and returns the variant to send, so
    /// both failure legs — the loop stopped, the loop dropped the channel —
    /// collapse here instead of being restated at each request method below.
    async fn call<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<T>) -> SessionRequest,
    ) -> anyhow::Result<T> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.node.send(build(resp_tx)).await?;
        resp_rx
            .await
            .map_err(|_| anyhow::anyhow!("mesh event loop dropped the response"))
    }

    /// Create a new mesh as a poll-only, silent core (the MCP server).
    ///
    /// # Errors
    /// [`CreateError::AdvertiseRequiresReachable`] / [`CreateError::Setup`].
    pub(crate) async fn create_poll(cfg: CreateConfig) -> Result<Self, CreateError> {
        let (elc, io, advertiser) = create_setup(cfg, Output::silent()).await?;
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
    pub(crate) async fn join_poll(cfg: JoinConfig) -> Result<Self, JoinError> {
        let (elc, io) = join_setup(cfg, Output::silent()).await?;
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
    pub(crate) async fn topic_poll(cfg: TopicConfig) -> Result<Self, JoinError> {
        let (elc, io) = topic_setup(cfg, Output::silent()).await?;
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
    pub(crate) async fn send(&self, body: MessageBody) -> anyhow::Result<Message> {
        // Two Results: the outer `?` is the channel, the inner is the loop's answer.
        self.call(|resp| SessionRequest::Send { body, resp })
            .await?
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
    ) -> anyhow::Result<crate::a2a::surfaced::PollBatch> {
        self.call(|resp| SessionRequest::Poll { after, long, resp })
            .await
    }

    /// Worker-emit a `TaskStatusUpdate` on a task we're serving; returns the
    /// canonical [`Message`].
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub(crate) async fn task_status(
        &self,
        task_id: TaskId,
        state: crate::a2a::TaskState,
        note: Option<String>,
    ) -> anyhow::Result<Message> {
        self.call(|resp| SessionRequest::TaskStatus {
            task_id,
            state,
            note,
            resp,
        })
        .await?
    }

    /// Worker-emit a `TaskArtifactUpdate` (the result) on a task we're serving.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub(crate) async fn task_artifact(
        &self,
        task_id: TaskId,
        text: String,
        file: Option<agent_habilis_mesh::blob::FileRef>,
    ) -> anyhow::Result<Message> {
        self.call(|resp| SessionRequest::TaskArtifact {
            task_id,
            text,
            file,
            resp,
        })
        .await?
    }

    /// Call a peer's A2A server over gossip (request/response) and return the
    /// parsed JSON-RPC response object (`{"result"|"error"}`). Blocks until
    /// the peer answers or `timeout` elapses.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub(crate) async fn a2a_call(&self, call: A2aCallParams) -> anyhow::Result<serde_json::Value> {
        let A2aCallParams {
            peer,
            method,
            params,
            timeout,
        } = call;
        self.call(|resp| SessionRequest::A2aCall {
            peer,
            method,
            params,
            timeout,
            resp,
        })
        .await
    }

    /// Snapshot the live participant roster (active + quiet, recency-sorted).
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub(crate) async fn peers(&self) -> anyhow::Result<RosterSnapshot> {
        self.call(|resp| SessionRequest::Peers { resp }).await
    }

    /// Run an RTT round and return the per-peer round-trip rows. Blocks for
    /// the ping window (the round finalizes on its deadline), so a quiet mesh
    /// returns an empty list after that delay.
    pub(crate) async fn ping(&self) -> anyhow::Result<Vec<crate::output::PingPeer>> {
        let rows = self.call(|resp| SessionRequest::Ping { resp }).await?;
        // Map the engine's chat-agnostic RTT rows onto the app's public ping
        // datum — same fields, distinct layers.
        Ok(rows
            .into_iter()
            .map(|row| crate::output::PingPeer {
                nickname: row.nickname,
                rtt_ms: row.rtt_ms,
            })
            .collect())
    }

    /// Apply an RFC 7386 JSON Merge Patch to the shared state. Any JSON value is a
    /// valid merge; `Err` is a transport/serialize failure only.
    pub(crate) async fn state_merge(&self, merge: serde_json::Value) -> anyhow::Result<()> {
        self.call(|resp| SessionRequest::StateMerge { merge, resp })
            .await?
    }

    /// The current derived shared-state document (the merge fold).
    pub(crate) async fn state_get(&self) -> anyhow::Result<serde_json::Value> {
        self.call(|resp| SessionRequest::StateGet { resp }).await
    }

    /// `meta`-channel counterpart of [`state_merge`](Self::state_merge).
    pub(crate) async fn meta_merge(&self, merge: serde_json::Value) -> anyhow::Result<()> {
        self.call(|resp| SessionRequest::MetaMerge { merge, resp })
            .await?
    }

    /// `meta`-channel counterpart of [`state_get`](Self::state_get).
    pub(crate) async fn meta_get(&self) -> anyhow::Result<serde_json::Value> {
        self.call(|resp| SessionRequest::MetaGet { resp }).await
    }

    /// Broadcast pre-built wire bytes verbatim (no signing/chain). Testkit
    /// only — the injection point for crafted/malicious messages.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    #[cfg(feature = "adversarial")]
    pub(crate) async fn inject_raw(&self, bytes: bytes::Bytes) -> anyhow::Result<()> {
        self.node.send(SessionRequest::InjectRaw { bytes }).await
    }

    /// This node's iroh endpoint id (testkit).
    #[cfg(feature = "adversarial")]
    pub(crate) fn endpoint_id(&self) -> iroh::EndpointId {
        self.endpoint_id
    }

    /// This node's X25519 circuit key (testkit) — a peer needs it to onion-seal
    /// a circuit terminating here.
    #[cfg(feature = "adversarial")]
    pub(crate) fn circuit_key(&self) -> [u8; 32] {
        self.circuit_key
    }

    /// Ingest a synthetic link-state vector into this node's multihop routing
    /// table (testkit) — stands up a topology a live rendezvous mesh won't
    /// converge. See [`iroh_multihop_transport::Topology`].
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    #[cfg(feature = "adversarial")]
    pub(crate) async fn inject_link_vector(
        &self,
        origin: iroh::EndpointId,
        seq: u64,
        links: Vec<(iroh::EndpointId, u32)>,
    ) -> anyhow::Result<()> {
        self.node
            .send(SessionRequest::InjectLinkVector { origin, seq, links })
            .await
    }

    /// Simulate the gossip stream terminally ending. Adversarial-suite
    /// only — drives the stream-end resubscribe recovery test.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    #[cfg(feature = "adversarial")]
    pub(crate) async fn sever_gossip(&self) -> anyhow::Result<()> {
        self.node.send(SessionRequest::SeverGossip).await
    }

    /// Snapshot the fork/DAG index sizes `(by_hash, dag_heads, author_seqs)`.
    /// Adversarial-suite only.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    #[cfg(feature = "adversarial")]
    pub(crate) async fn index_stats(&self) -> anyhow::Result<(usize, usize, usize)> {
        self.call(|resp| SessionRequest::IndexStats { resp }).await
    }

    /// Snapshot the reassembly store's accounting
    /// `(groups, total_bytes, max_author_bytes)`. Adversarial-suite only.
    #[cfg(feature = "adversarial")]
    pub(crate) async fn reassembly_stats(&self) -> anyhow::Result<(usize, usize, usize)> {
        self.call(|resp| SessionRequest::ReassemblyStats { resp })
            .await
    }

    /// Clean shutdown: ask the loop to broadcast `Left` and wind down,
    /// waiting up to 3s. On timeout returns `Ok(())` and `Drop` detaches.
    ///
    /// # Errors
    /// Returns an error if the event-loop task panicked or returned an error.
    pub(crate) async fn leave(self) -> anyhow::Result<()> {
        let Self {
            node, advertiser, ..
        } = self;
        // Stop advertising first — we're leaving, so the listing should age out
        // rather than keep being re-broadcast while the loop winds down.
        drop(advertiser);
        node.leave().await
    }
}
