use agent_habilis_mesh::embed::RosterSnapshot;
use agent_habilis_mesh::runtime::{SetupKind, SetupParams, setup_mesh};
use agent_habilis_mesh::util::consts::GOSSIP_ACTIVE_VIEW_CAPACITY;
use tokio::sync::{broadcast, mpsc};

use super::advertise::Advertiser;
use super::config::{CreateConfig, JoinConfig, TopicConfig};
use super::error::{CreateError, JoinError};
use super::inproc::InProcessSession;
use super::params::{A2aCallParams, TaskArtifactParams};
use super::setup::{SpawnEnv, create_setup, join_setup, topic_setup};
use crate::a2a::TaskId;
use crate::output::OutputEvent;
use crate::output::PingPeer;
use agent_habilis_mesh::protocol::{Mesh, MeshName};
use agent_habilis_mesh::protocol::{MeshId, Message, MessageBody, Nickname};
use agent_habilis_mesh::runtime::tuning::NODE_INBOUND_CAP;
use agent_habilis_mesh::runtime::{CoHostPolicy, EventLoopConfig};

/// A live mesh membership (the public api): the shared
/// `InProcessSession` plus the inbound broadcast and captured-event
/// stream. Dropping it (or [`MeshSession::leave`]) winds the loop down.

#[derive(Debug)]
pub struct MeshSession {
    pub(crate) core: InProcessSession,
    msg_tx: broadcast::Sender<Message>,
    /// Captured structured output events. Single-consumer (mpsc), so
    /// `events()` hands it out exactly once.
    events_rx: Option<mpsc::UnboundedReceiver<OutputEvent>>,
}

impl MeshSession {
    /// Resolve `cfg.target`, join the mesh, and spawn the event loop in the
    /// background. Output is captured per-session into
    /// [`MeshSession::events`] (the embedder owns stdout/stderr).
    ///
    /// # Errors
    /// [`JoinError::Resolve`] if the target can't be resolved;
    /// [`JoinError::Setup`] on endpoint/gossip failure.
    pub async fn join(cfg: JoinConfig) -> Result<Self, JoinError> {
        let (output, events_rx) = crate::output::capture_events();
        let (elc, io) = join_setup(cfg, output).await?;
        Ok(Self::with_events(elc, io, None, events_rx))
    }

    /// Join a topic — a public mesh derived deterministically from
    /// `cfg.string` — and spawn its event loop in the background. Output is
    /// captured per-session into [`MeshSession::events`].
    ///
    /// # Errors
    /// [`JoinError::Resolve`] if `cfg.string` is empty/whitespace;
    /// [`JoinError::Setup`] on endpoint/gossip failure.
    pub async fn topic(cfg: TopicConfig) -> Result<Self, JoinError> {
        let (output, events_rx) = crate::output::capture_events();
        let (elc, io) = topic_setup(cfg, output).await?;
        Ok(Self::with_events(elc, io, None, events_rx))
    }

    /// Create a new mesh and spawn its event loop in the background.
    /// `cfg.lookups` is resolved the same granular way the CLI uses.
    ///
    /// # Errors
    /// [`CreateError::AdvertiseRequiresReachable`] if `advertise` is set on a
    /// loopback-only mesh; [`CreateError::Setup`] on endpoint/gossip failure.
    pub async fn create(cfg: CreateConfig) -> Result<Self, CreateError> {
        let (output, events_rx) = crate::output::capture_events();
        let (elc, io, advertiser) = create_setup(cfg, output).await?;
        Ok(Self::with_events(elc, io, advertiser, events_rx))
    }

    /// Join an already-decoded [`Mesh`] with an explicit co-host policy —
    /// the internal directory-session path (the advertiser eager-cohosts; the
    /// discover consumer never cohosts). `pub(crate)`: keeps `Mesh` off the
    /// iroh-free surface. Directory sessions never register process signal
    /// handlers (see [`DriverMode::InProcess`]) — the hosting command owns
    /// its own lifetime, and hijacking ctrl-c would keep it alive.
    ///
    /// # Errors
    /// Fails if endpoint/gossip setup fails.
    pub(crate) async fn join_decoded(
        mesh: Mesh,
        nickname: Option<Nickname>,
        cohost: CoHostPolicy,
    ) -> anyhow::Result<Self> {
        crate::register_build_version();
        let author = nickname.unwrap_or_else(Nickname::random);
        let (output, events_rx) = crate::output::capture_events();
        let io = crate::a2a::app::SurfacedIo::new(output);
        let sink = io.sink();
        let elc = setup_mesh(
            // Directory sessions (advertise/discover) never offload blobs, so no
            // password needs threading here.
            SetupKind::Join {
                mesh,
                password: None,
            },
            SetupParams {
                author,
                max_peers: GOSSIP_ACTIVE_VIEW_CAPACITY,
                runtime_base: Some(crate::runtime_base()),
                state_file: None,
                sink,
                per_peer_gate: Some(crate::a2a::card_gate()),
                multihop: false,
                cohost: Some(cohost),
                live_count: None,
            },
        )
        .await?;
        Ok(Self::with_events_and_signals(
            elc,
            io,
            SpawnEnv {
                advertiser: None,
                handle_signals: false,
            },
            events_rx,
        ))
    }

    /// The events presentation: a broadcast for inbound traffic plus the
    /// captured-event stream, over a freshly-spawned [`InProcessSession`]
    /// (which pushes inbound to the broadcast). Registers the process
    /// signal handlers (the public api default).
    fn with_events(
        elc: EventLoopConfig,
        io: crate::a2a::app::SurfacedIo,
        advertiser: Option<Advertiser>,
        events_rx: mpsc::UnboundedReceiver<OutputEvent>,
    ) -> Self {
        Self::with_events_and_signals(
            elc,
            io,
            SpawnEnv {
                advertiser,
                handle_signals: true,
            },
            events_rx,
        )
    }

    /// [`Self::with_events`] with the signal registration explicit — the
    /// directory sessions pass `handle_signals: false`.
    fn with_events_and_signals(
        elc: EventLoopConfig,
        io: crate::a2a::app::SurfacedIo,
        env: SpawnEnv,
        events_rx: mpsc::UnboundedReceiver<OutputEvent>,
    ) -> Self {
        let (msg_tx, _initial_rx) = broadcast::channel::<Message>(NODE_INBOUND_CAP);
        let core = InProcessSession::spawn(elc, io, env, Some(msg_tx.clone()));
        Self {
            core,
            msg_tx,
            events_rx: Some(events_rx),
        }
    }

    /// Take the captured structured-event stream ([`OutputEvent`]: `ready`,
    /// `message`, `presence`, `peer_*`, `info`, …). Single-consumer, so this
    /// returns the receiver **once**; later calls return `None`.
    pub fn events(&mut self) -> Option<mpsc::UnboundedReceiver<OutputEvent>> {
        self.events_rx.take()
    }

    /// The resolved mesh identifier.
    #[must_use]
    pub fn mesh_id(&self) -> &MeshId {
        self.core.mesh_id()
    }

    /// The mesh's human-readable name (decoded from the id).
    #[must_use]
    pub fn name(&self) -> &MeshName {
        self.core.name()
    }

    /// Our effective nickname in this mesh.
    #[must_use]
    pub fn nickname(&self) -> &Nickname {
        self.core.nickname()
    }

    /// Subscribe to inbound messages. Each call returns an independent
    /// receiver that sees traffic sent *after* it subscribed, so subscribe
    /// before you expect messages. Includes every kind that parses — `msg`,
    /// `presence` (joined/left/alive), `peer_info`; filter as needed. Under
    /// sustained lag the bounded ring drops the oldest messages and the
    /// receiver observes [`broadcast::error::RecvError::Lagged`] — by design,
    /// so a slow consumer never stalls the gossip loop.
    #[must_use]
    pub fn messages(&self) -> broadcast::Receiver<Message> {
        self.msg_tx.subscribe()
    }

    /// Build, sign and gossip-broadcast a mesh chat message. Returns the
    /// canonical [`Message`] the loop built (read `.id` for the new id).
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub async fn broadcast(&self, body: MessageBody) -> anyhow::Result<Message> {
        self.core.broadcast(body).await
    }

    /// Send a msg: a chat message to one peer. Only you and `to`
    /// see it: the frame is delivered point-to-point and sealed to the
    /// recipient, so the peers relaying it cannot read the body.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response, or if the
    /// peer's seal key has not replicated yet (a msg is never sent in plaintext).
    pub async fn msg(&self, to: Nickname, body: MessageBody) -> anyhow::Result<Message> {
        self.core.msg(to, body).await
    }

    /// Apply an RFC 7386 JSON Merge Patch to the shared state: an object merges
    /// into the document (a `null` member deletes its key; nested objects merge
    /// recursively), and a non-object value replaces the target.
    ///
    /// # Errors
    /// Fails if the event loop has stopped (a merge always applies).
    pub async fn state_merge(&self, merge: serde_json::Value) -> anyhow::Result<()> {
        self.core.state_merge(merge).await
    }

    /// The current derived shared-state document (the merge fold over the state
    /// log).
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub async fn state_get(&self) -> anyhow::Result<serde_json::Value> {
        self.core.state_get().await
    }

    /// Apply an RFC 7386 JSON Merge Patch to the **meta** channel (the
    /// mesh-metadata counterpart of [`state_merge`](Self::state_merge)).
    ///
    /// # Errors
    /// As [`state_merge`](Self::state_merge).
    pub async fn meta_merge(&self, merge: serde_json::Value) -> anyhow::Result<()> {
        self.core.meta_merge(merge).await
    }

    /// The current derived **meta**-channel document.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub async fn meta_get(&self) -> anyhow::Result<serde_json::Value> {
        self.core.meta_get().await
    }

    /// Poll the surfaced-event history after the `after` seq cursor (`None`
    /// for the full buffered window). Join-horizon filtered. A pull
    /// alternative to the [`MeshSession::messages`] live subscription that
    /// surfaces *every* event kind (chat, presence, task legs, and the
    /// transient `ping_report` / `peer_timeout` / … events), each tagged with
    /// its surfacing `seq` — pass the last returned `seq` as the next `after`.
    ///
    /// `long` long-polls: when the buffer is empty, park the read up to the
    /// server cap (~60s), returning early on the first new event — an empty
    /// batch at the deadline just means the window elapsed quietly; call
    /// again. `false` is the immediate read.
    ///
    /// Check [`PollBatch::missed_before`](crate::a2a::surfaced::PollBatch): when
    /// set, the cursor aged out of the ring and every event below that seq was
    /// lost. Re-baseline on the returned window.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub async fn fetch(
        &self,
        after: Option<u64>,
        long: bool,
    ) -> anyhow::Result<crate::a2a::surfaced::PollBatch> {
        self.core.fetch(after, long).await
    }

    /// Worker-emit a `TaskStatusUpdate` on a task we're serving (the A2A
    /// streaming plane) — `working` / `input-required` / `completed` /
    /// `failed`. Returns the canonical [`Message`] the loop built.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub async fn task_status(
        &self,
        task_id: TaskId,
        state: crate::a2a::TaskState,
        note: Option<String>,
    ) -> anyhow::Result<Message> {
        self.core.task_status(task_id, state, note).await
    }

    /// Worker-emit a `TaskArtifactUpdate` (the result) on a task we're serving.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub async fn task_artifact(&self, artifact: TaskArtifactParams) -> anyhow::Result<Message> {
        let TaskArtifactParams {
            task_id,
            text,
            file,
            file_name,
            file_mime,
        } = artifact;
        let file = file.map(|path| crate::a2a::send::FileRef {
            path,
            name: file_name,
            mime: file_mime,
        });
        self.core.task_artifact(task_id, text, file).await
    }

    /// Call a peer's A2A server over gossip (request/response). Returns the
    /// parsed JSON-RPC response (`{"result"|"error"}`); blocks until the peer
    /// answers or `timeout` elapses. The peer serves a safe method subset
    /// (reads, party-checked `tasks/cancel`, and `message/send` directed at
    /// it) — mutating global-state ops and broadcast sends are refused.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub async fn a2a_call(&self, call: A2aCallParams) -> anyhow::Result<serde_json::Value> {
        self.core.a2a_call(call).await
    }

    /// Snapshot the live peer roster (active + quiet, recency-sorted).
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub async fn peers(&self) -> anyhow::Result<RosterSnapshot> {
        self.core.peers().await
    }

    /// Run an RTT round and return the per-peer round-trip rows. Blocks for the
    /// ping window (the round finalizes on its deadline), so a quiet mesh
    /// returns an empty list after that delay.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub async fn ping(&self) -> anyhow::Result<Vec<PingPeer>> {
        self.core.ping().await
    }

    /// Clean shutdown: ask the loop to broadcast `Left` and wind down,
    /// waiting up to 3s. On timeout returns `Ok(())` and the task detaches.
    ///
    /// # Errors
    /// Returns an error if the event-loop task panicked or returned an error.
    pub async fn leave(self) -> anyhow::Result<()> {
        self.core.leave().await
    }
}
