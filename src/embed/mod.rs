//! In-process embedding facade.
//!
//! [`SwarmSession`] runs the swarm event loop as a background `tokio`
//! task **inside the caller's process** — no subprocess, no Unix-socket
//! IPC. Inbound traffic is pushed over a bounded broadcast channel;
//! outbound sends go through a dedicated channel into the same shared
//! broadcast path the CLI/IPC uses. No `iroh` type is exposed: a join
//! target is a `💬…` id parsed from a string at the boundary.

use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::a2a::TaskId;
use crate::a2a::session::SessionRequest;
use crate::output::{Output, OutputEvent};
use agent_habilis_gossip::daemon::setup::{SetupKind, SetupParams, setup_swarm};
use agent_habilis_gossip::daemon::state::RosterSnapshot;
use agent_habilis_gossip::daemon::{
    CoHostPolicy, CreateParams, DriverMode, EventLoopConfig, JoinParams, Resolved, TopicParams,
};
use agent_habilis_gossip::directory::{self, Listing, ListingChange, Listings, directory_swarm};
use agent_habilis_gossip::protocol::swarm::{
    DEFAULT_DIRECTORY, DirectorySelection, LookupOpts, LookupSet, Swarm, SwarmConfig, SwarmName,
    resolve_lookups,
};
use agent_habilis_gossip::protocol::{Message, MessageBody, Nickname, SwarmId};
use agent_habilis_gossip::resolver::JoinTarget;
use agent_habilis_gossip::transport::TransportPolicy;
use agent_habilis_gossip::util::consts::GOSSIP_ACTIVE_VIEW_CAPACITY;
use agent_habilis_gossip::util::tuning::{
    EMBED_INBOUND_CAP, advertise_interval_secs, directory_expiry_secs,
};

/// How to join a swarm.
#[derive(Debug, Clone)]
pub struct JoinConfig {
    /// What to join: an `💬…` id, classified into a [`JoinTarget`] at the
    /// boundary (parse a string with [`str::parse`]). The network mode and
    /// name are decoded from the id. (A shared *string* derives its own
    /// swarm — see the `topic` command — and is not a join target.)
    pub target: JoinTarget,
    /// Local nickname. `None` mints a random `word-word` one.
    pub nickname: Option<Nickname>,
    /// Max direct peer connections before gossip relays the rest.
    pub max_peers: usize,
    /// Password for a password-protected id. Verified locally against the
    /// id's verifier before any network; required iff the id carries one.
    pub password: Option<String>,
    /// Which transport planes directed sends may use. Default all-on;
    /// narrowed by tests to pin a message to one lane.
    pub transport: TransportPolicy,
    /// Mirror frames into this shared directory (and ingest frames peers write
    /// there) — the filesystem spool. `None` disables it.
    pub spool: Option<std::path::PathBuf>,
}

impl JoinConfig {
    /// A config for `target` with a random nickname and the default
    /// peer cap. Set [`JoinConfig::nickname`] / [`JoinConfig::max_peers`]
    /// afterwards to override. Build the [`JoinTarget`] by parsing a
    /// string (`"💬…".parse()?`).
    #[must_use]
    pub fn new(target: JoinTarget) -> Self {
        Self {
            target,
            nickname: None,
            max_peers: GOSSIP_ACTIVE_VIEW_CAPACITY,
            password: None,
            transport: TransportPolicy::default(),
            spool: None,
        }
    }
}

/// How to join a **topic**: a public swarm derived deterministically from a
/// shared string. The name and (always-public) config are derived from the
/// string, so it is the only input — anyone passing the same string converges
/// on the same swarm.
#[derive(Debug, Clone)]
pub struct TopicConfig {
    /// The shared string. Hashed into the swarm seed after trimming
    /// surrounding whitespace (never case-folded).
    pub string: String,
    /// Local nickname. `None` mints a random `word-word` one.
    pub nickname: Option<Nickname>,
    /// Max direct peer connections before gossip relays the rest.
    pub max_peers: usize,
    /// Which transport planes directed sends may use. Default all-on;
    /// narrowed by tests to pin a message to one lane.
    pub transport: TransportPolicy,
    /// Mirror frames into this shared directory (and ingest frames peers write
    /// there) — the filesystem spool. `None` disables it.
    pub spool: Option<std::path::PathBuf>,
}

impl TopicConfig {
    /// A config for `string` with a random nickname and the default peer cap.
    #[must_use]
    pub fn new(string: String) -> Self {
        Self {
            string,
            nickname: None,
            max_peers: GOSSIP_ACTIVE_VIEW_CAPACITY,
            transport: TransportPolicy::default(),
            spool: None,
        }
    }
}

/// How to create a new swarm. Built from validated domain types
/// ([`SwarmName`], [`Nickname`], [`LookupSet`]); the iroh `RelayUrl`
/// stays hidden behind [`RelayLadder`](crate::RelayLadder) inside the
/// lookups, so the surface is iroh-free.
#[derive(Debug, Clone)]
pub struct CreateConfig {
    /// The swarm name (validated): 1..=32 UTF-8 characters (any
    /// script/emoji), excluding control characters, whitespace, and any
    /// of `/ \ < > #`.
    pub name: SwarmName,
    /// Local nickname. `None` mints a random `word-word` one.
    pub nickname: Option<Nickname>,
    /// `true` ⇒ the all-on lookup preset (mDNS + DHT + default relay
    /// ladder) when `lookups` names nothing; `false` ⇒ loopback only.
    /// Sugar over `lookups`, mirroring the CLI `--public`. Default `false`.
    pub public: bool,
    /// Granular lookup allowlist (`mdns`/`dht`/`relay`). Naming any one
    /// uses *only* those (relay defaults off); naming none falls back to
    /// `public`. Default [`LookupSet::default`] (all off). Mirrors the
    /// CLI `--mdns`/`--dht`/`--relay` flags.
    pub lookups: LookupSet,
    /// List this swarm in a directory so discoverers can find it
    /// without its `💬…` id. Requires `public`. Default `false`.
    pub advertise: bool,
    /// The directory to advertise into when `advertise` is set.
    /// `None` ⇒ the well-known `global` directory.
    pub directory: Option<SwarmName>,
    /// Max direct peer connections before gossip relays the rest.
    pub max_peers: usize,
    /// Protect the swarm with a password: its verifier is baked into the
    /// minted id (joiners must present the password), and every derivation
    /// switches onto the Argon2id-stretched key.
    pub password: Option<String>,
    /// Which transport planes directed sends may use. Default all-on;
    /// narrowed by tests to pin a message to one lane.
    pub transport: TransportPolicy,
    /// Mirror frames into this shared directory (and ingest frames peers write
    /// there) — the filesystem spool. `None` disables it.
    pub spool: Option<std::path::PathBuf>,
}

impl CreateConfig {
    /// A private-network config for swarm `name` with a random
    /// nickname and the default peer cap. Set the other fields
    /// afterwards to override.
    #[must_use]
    pub fn new(name: SwarmName) -> Self {
        Self {
            name,
            nickname: None,
            public: false,
            lookups: LookupSet::default(),
            advertise: false,
            directory: None,
            max_peers: GOSSIP_ACTIVE_VIEW_CAPACITY,
            password: None,
            transport: TransportPolicy::default(),
            spool: None,
        }
    }
}

/// Why [`SwarmSession::create`] failed, classified so callers can react:
/// the MCP server maps [`CreateError::AdvertiseRequiresReachable`] to an
/// `invalid_params` error and [`CreateError::Setup`] to an internal one.
#[derive(Debug)]
pub enum CreateError {
    /// `advertise` was requested on a loopback-only swarm — a directory
    /// listing requires a swarm reachable across machines.
    AdvertiseRequiresReachable,
    /// Endpoint / gossip / setup failure.
    Setup(anyhow::Error),
}

impl fmt::Display for CreateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Single source of truth for the message: the validator's error.
            CreateError::AdvertiseRequiresReachable => {
                write!(
                    formatter,
                    "{}",
                    agent_habilis_gossip::protocol::swarm::AdvertiseRequiresReachable
                )
            }
            CreateError::Setup(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for CreateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CreateError::AdvertiseRequiresReachable => None,
            CreateError::Setup(error) => {
                let source: &(dyn std::error::Error + 'static) = error.as_ref();
                Some(source)
            }
        }
    }
}

/// Why [`SwarmSession::join`] failed — the symmetric counterpart to
/// [`CreateError`]. `Resolve` is a malformed `💬…` id; `Setup` is an
/// endpoint/gossip failure. The MCP server maps both to an internal error.
#[derive(Debug)]
pub enum JoinError {
    /// The target could not be resolved into a swarm.
    Resolve(anyhow::Error),
    /// Endpoint / gossip / setup failure.
    Setup(anyhow::Error),
}

impl fmt::Display for JoinError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JoinError::Resolve(error) | JoinError::Setup(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for JoinError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        let (JoinError::Resolve(error) | JoinError::Setup(error)) = self;
        let source: &(dyn std::error::Error + 'static) = error.as_ref();
        Some(source)
    }
}

/// The captured-output sink plus its single-consumer receiver — the embed
/// facade's [`SwarmSession::events`] stream.
fn capture() -> (Output, mpsc::UnboundedReceiver<OutputEvent>) {
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    (Output::capture(events_tx), events_rx)
}

/// Resolve + set up a create: the ready [`EventLoopConfig`] plus the spawned
/// directory advertiser task (if `advertise` was requested). The caller picks
/// the `output` sink (captured for [`SwarmSession`], silent for the MCP core).
///
/// # Errors
/// [`CreateError::AdvertiseRequiresReachable`] / [`CreateError::Setup`].
async fn create_setup(
    cfg: CreateConfig,
    output: Output,
) -> Result<
    (
        EventLoopConfig,
        crate::a2a::app::SurfacedIo,
        Option<JoinHandle<()>>,
    ),
    CreateError,
> {
    let config = SwarmConfig {
        lookups: resolve_lookups(cfg.public, cfg.lookups),
        password: None,
        issuer_pubkey: None,
    };
    // The advertiser reaches the directory over this swarm's own lookups.
    let directory_lookups = config.lookups.clone();
    // Map embed's `advertise: bool` + `directory` onto the shared
    // `DirectorySelection`; `resolve` validates it against the config
    // (loopback-only advertise is rejected before any setup work).
    let advertise = match (cfg.advertise, cfg.directory) {
        (false, _) => DirectorySelection::Unset,
        (true, None) => DirectorySelection::Default,
        (true, Some(directory)) => DirectorySelection::Named(directory),
    };
    let max_peers = cfg.max_peers;
    let Resolved {
        kind,
        author,
        advertise_directory,
    } = CreateParams {
        name: cfg.name,
        nickname: cfg.nickname,
        config,
        advertise,
        password: cfg
            .password
            .map(agent_habilis_gossip::protocol::crypto::Password::new),
        // Invite-only is a CLI-driven feature; the embed facade does not expose
        // it yet (a documented follow-up).
        invite_only: false,
    }
    .resolve()
    .map_err(|_| CreateError::AdvertiseRequiresReachable)?;
    let io = crate::a2a::app::SurfacedIo::new(output);
    let sink = io.sink();
    let mut elc = setup_swarm(
        kind,
        SetupParams {
            author,
            interactive: false,
            max_peers,
            state_file: None,
            spool: cfg.spool,
            sink,
            transport: cfg.transport,
            drift: None,
            a2a_serve: None,
        },
    )
    .await
    .map_err(|error| CreateError::Setup(error.context("setup_swarm failed")))?;
    // When advertising, start the re-broadcast task (tied to this session);
    // it reaches the directory over this swarm's own lookups (moved into the
    // at-most-once closure, so no clone).
    let advertiser = advertise_directory
        .map(|directory| spawn_advertiser(&mut elc, directory, directory_lookups));
    Ok((elc, io, advertiser))
}

/// Resolve + set up a join: the ready [`EventLoopConfig`]. The caller picks
/// the `output` sink.
///
/// # Errors
/// [`JoinError::Resolve`] / [`JoinError::Setup`].
async fn join_setup(
    cfg: JoinConfig,
    output: Output,
) -> Result<(EventLoopConfig, crate::a2a::app::SurfacedIo), JoinError> {
    let resolved = JoinParams {
        target: cfg.target,
        nickname: cfg.nickname,
        password: cfg
            .password
            .map(agent_habilis_gossip::protocol::crypto::Password::new),
    }
    .resolve()
    .map_err(JoinError::Resolve)?;
    resolved_setup(resolved, cfg.max_peers, cfg.spool, cfg.transport, output).await
}

/// Resolve + set up a topic (a string-derived public swarm).
///
/// # Errors
/// [`JoinError::Resolve`] if the string is empty/whitespace;
/// [`JoinError::Setup`] on endpoint/gossip failure.
async fn topic_setup(
    cfg: TopicConfig,
    output: Output,
) -> Result<(EventLoopConfig, crate::a2a::app::SurfacedIo), JoinError> {
    let resolved = TopicParams {
        string: cfg.string,
        nickname: cfg.nickname,
    }
    .resolve()
    .map_err(JoinError::Resolve)?;
    resolved_setup(resolved, cfg.max_peers, cfg.spool, cfg.transport, output).await
}

/// The shared tail of [`join_setup`] / [`topic_setup`]: run `setup_swarm` for
/// an already-resolved join-flavored setup.
async fn resolved_setup(
    resolved: Resolved,
    max_peers: usize,
    spool: Option<std::path::PathBuf>,
    transport: TransportPolicy,
    output: Output,
) -> Result<(EventLoopConfig, crate::a2a::app::SurfacedIo), JoinError> {
    let Resolved { kind, author, .. } = resolved;
    let io = crate::a2a::app::SurfacedIo::new(output);
    let sink = io.sink();
    let elc = setup_swarm(
        kind,
        SetupParams {
            author,
            interactive: false,
            max_peers,
            state_file: None,
            spool,
            sink,
            transport,
            drift: None,
            a2a_serve: None,
        },
    )
    .await
    .map_err(|error| JoinError::Setup(error.context("setup_swarm failed")))?;
    Ok((elc, io))
}

/// The in-process session core shared by the public [`SwarmSession`] (embed
/// facade) and the MCP server. Owns the typed request channel, the quit
/// signal, the event-loop task, and the optional advertiser, and drives
/// `send`/`fetch`/`leave`. The two frontends differ only in *presentation*:
/// `SwarmSession` adds the inbound broadcast + captured-event stream; the
/// MCP server wraps this core directly (poll-only, silent output).
#[derive(Debug)]
pub(crate) struct InProcessSession {
    swarm_id: SwarmId,
    name: SwarmName,
    nickname: Nickname,
    /// This node's iroh endpoint id + X25519 circuit key, captured for the
    /// testkit `inject_link_vector` (which needs a peer's id + key to build the
    /// synthetic link-state vector).
    #[cfg(feature = "adversarial")]
    endpoint_id: iroh::EndpointId,
    #[cfg(feature = "adversarial")]
    circuit_key: [u8; 32],
    req_tx: mpsc::Sender<SessionRequest>,
    quit_tx: mpsc::Sender<()>,
    task: Option<JoinHandle<anyhow::Result<()>>>,
    /// Directory re-broadcast task (when created with `advertise`); aborted
    /// on `leave`/drop so we stop advertising a swarm we left.
    advertiser: Option<JoinHandle<()>>,
}

impl InProcessSession {
    /// Wire the typed channels into `elc`, spawn the event loop, and build
    /// the core. `push` is `Some` to fan inbound traffic out to a broadcast
    /// ([`SwarmSession::messages`]), `None` for a poll-only consumer (MCP).
    /// `handle_signals`: see [`DriverMode::InProcess`] — `false` for sessions
    /// living inside a foreground command that owns its own lifetime.
    fn spawn(
        mut elc: EventLoopConfig,
        io: crate::a2a::app::SurfacedIo,
        advertiser: Option<JoinHandle<()>>,
        push: Option<broadcast::Sender<Message>>,
        handle_signals: bool,
    ) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<SessionRequest>(32);
        let (quit_tx, quit_rx) = mpsc::channel::<()>(1);
        elc.driver = DriverMode::InProcess {
            msg_tx: push,
            quit_rx,
            handle_signals,
        };
        // The tapped `io` (built in `*_setup`) already backs `elc.sink`: the
        // engine emits `MeshEvent`s through `io.sink()`, and the app renders the
        // same tap. Build the app from that same `io` so surfaced events reach
        // the app-side ring the in-process `Poll` drains.
        let app = crate::a2a::app::A2aApp::with_io(io);
        let swarm_id = elc.swarm.clone();
        let name = elc.name.clone();
        let nickname = elc.author.clone();
        #[cfg(feature = "adversarial")]
        let endpoint_id = elc.endpoint.id();
        #[cfg(feature = "adversarial")]
        let circuit_key = elc.identity.seal_public();
        let task = tokio::spawn(agent_habilis_gossip::daemon::run(
            elc,
            app,
            Some(req_rx),
            None::<mpsc::Receiver<crate::a2a::rpc::A2aRequest>>,
        ));
        Self {
            swarm_id,
            name,
            nickname,
            #[cfg(feature = "adversarial")]
            endpoint_id,
            #[cfg(feature = "adversarial")]
            circuit_key,
            req_tx,
            quit_tx,
            task: Some(task),
            advertiser,
        }
    }

    /// Create a new swarm as a poll-only, silent core (the MCP server).
    ///
    /// # Errors
    /// [`CreateError::AdvertiseRequiresReachable`] / [`CreateError::Setup`].
    pub(crate) async fn create_poll(cfg: CreateConfig) -> Result<Self, CreateError> {
        let (elc, io, advertiser) = create_setup(cfg, Output::silent()).await?;
        Ok(Self::spawn(elc, io, advertiser, None, true))
    }

    /// Join an existing swarm as a poll-only, silent core (the MCP server).
    ///
    /// # Errors
    /// [`JoinError::Resolve`] / [`JoinError::Setup`].
    pub(crate) async fn join_poll(cfg: JoinConfig) -> Result<Self, JoinError> {
        let (elc, io) = join_setup(cfg, Output::silent()).await?;
        Ok(Self::spawn(elc, io, None, None, true))
    }

    /// Join a topic (string-derived public swarm) as a poll-only, silent core
    /// (the MCP server).
    ///
    /// # Errors
    /// [`JoinError::Resolve`] on an empty string; [`JoinError::Setup`] on
    /// endpoint/gossip failure.
    pub(crate) async fn topic_poll(cfg: TopicConfig) -> Result<Self, JoinError> {
        let (elc, io) = topic_setup(cfg, Output::silent()).await?;
        Ok(Self::spawn(elc, io, None, None, true))
    }

    pub(crate) fn swarm_id(&self) -> &SwarmId {
        &self.swarm_id
    }

    pub(crate) fn name(&self) -> &SwarmName {
        &self.name
    }

    pub(crate) fn nickname(&self) -> &Nickname {
        &self.nickname
    }

    /// Build, sign and gossip-broadcast a message; returns the canonical
    /// [`Message`] the loop built, or `None` when the sender-side rate
    /// limiter dropped it.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub(crate) async fn send(&self, body: MessageBody) -> anyhow::Result<Message> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.req_tx
            .send(SessionRequest::Send {
                body,
                resp: resp_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop has stopped"))?;
        match resp_rx.await {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!("swarm event loop dropped the response")),
        }
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
    ) -> anyhow::Result<Vec<crate::a2a::surfaced::SurfacedEvent>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.req_tx
            .send(SessionRequest::Poll {
                after,
                long,
                resp: resp_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop has stopped"))?;
        resp_rx
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop dropped the response"))
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
        let (resp_tx, resp_rx) = oneshot::channel();
        self.req_tx
            .send(SessionRequest::TaskStatus {
                task_id,
                state,
                note,
                resp: resp_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop has stopped"))?;
        match resp_rx.await {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!("swarm event loop dropped the response")),
        }
    }

    /// Worker-emit a `TaskArtifactUpdate` (the result) on a task we're serving.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub(crate) async fn task_artifact(
        &self,
        task_id: TaskId,
        text: String,
        file: Option<agent_habilis_gossip::blob::FileRef>,
    ) -> anyhow::Result<Message> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.req_tx
            .send(SessionRequest::TaskArtifact {
                task_id,
                text,
                file,
                resp: resp_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop has stopped"))?;
        match resp_rx.await {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!("swarm event loop dropped the response")),
        }
    }

    /// Call a peer's A2A server over gossip (request/response) and return the
    /// parsed JSON-RPC response object (`{"result"|"error"}`). Blocks until
    /// the peer answers or `timeout` elapses.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub(crate) async fn a2a_call(
        &self,
        peer: Nickname,
        method: String,
        params: serde_json::Value,
        timeout: Duration,
    ) -> anyhow::Result<serde_json::Value> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.req_tx
            .send(SessionRequest::A2aCall {
                peer,
                method,
                params,
                timeout,
                resp: resp_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop has stopped"))?;
        resp_rx
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop dropped the response"))
    }

    /// Snapshot the live participant roster (active + quiet, recency-sorted).
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub(crate) async fn peers(&self) -> anyhow::Result<RosterSnapshot> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.req_tx
            .send(SessionRequest::Peers { resp: resp_tx })
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop has stopped"))?;
        resp_rx
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop dropped the response"))
    }

    /// Run an RTT round and return the per-peer round-trip rows. Blocks for
    /// the ping window (the round finalizes on its deadline), so a quiet swarm
    /// returns an empty list after that delay.
    pub(crate) async fn ping(&self) -> anyhow::Result<Vec<crate::output::PingPeer>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.req_tx
            .send(SessionRequest::Ping { resp: resp_tx })
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop has stopped"))?;
        let rows = resp_rx
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop dropped the response"))?;
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
        let (resp_tx, resp_rx) = oneshot::channel();
        self.req_tx
            .send(SessionRequest::StateMerge {
                merge,
                resp: resp_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop has stopped"))?;
        resp_rx
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop dropped the response"))?
    }

    /// The current derived shared-state document (the merge fold).
    pub(crate) async fn state_get(&self) -> anyhow::Result<serde_json::Value> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.req_tx
            .send(SessionRequest::StateGet { resp: resp_tx })
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop has stopped"))?;
        resp_rx
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop dropped the response"))
    }

    /// `meta`-channel counterpart of [`state_merge`](Self::state_merge).
    pub(crate) async fn meta_merge(&self, merge: serde_json::Value) -> anyhow::Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.req_tx
            .send(SessionRequest::MetaMerge {
                merge,
                resp: resp_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop has stopped"))?;
        resp_rx
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop dropped the response"))?
    }

    /// `meta`-channel counterpart of [`state_get`](Self::state_get).
    pub(crate) async fn meta_get(&self) -> anyhow::Result<serde_json::Value> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.req_tx
            .send(SessionRequest::MetaGet { resp: resp_tx })
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop has stopped"))?;
        resp_rx
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop dropped the response"))
    }

    /// Broadcast pre-built wire bytes verbatim (no signing/chain). Testkit
    /// only — the injection point for crafted/malicious messages.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    #[cfg(feature = "adversarial")]
    pub(crate) async fn inject_raw(&self, bytes: bytes::Bytes) -> anyhow::Result<()> {
        self.req_tx
            .send(SessionRequest::InjectRaw { bytes })
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop has stopped"))
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

    /// Ingest a synthetic link-state vector into this node's routing graph
    /// (testkit) — stands up a circuit topology a live rendezvous mesh won't
    /// converge. See [`agent_habilis_gossip::circuit::LinkStateStore`].
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    #[cfg(feature = "adversarial")]
    pub(crate) async fn inject_link_vector(
        &self,
        origin: iroh::EndpointId,
        seq: u64,
        seal_key: [u8; 32],
        links: Vec<(iroh::EndpointId, u32)>,
    ) -> anyhow::Result<()> {
        self.req_tx
            .send(SessionRequest::InjectLinkVector {
                origin,
                seq,
                seal_key,
                links,
            })
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop has stopped"))
    }

    /// Simulate the gossip stream terminally ending. Adversarial-suite
    /// only — drives the stream-end resubscribe recovery test.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    #[cfg(feature = "adversarial")]
    pub(crate) async fn sever_gossip(&self) -> anyhow::Result<()> {
        self.req_tx
            .send(SessionRequest::SeverGossip)
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop has stopped"))
    }

    /// Snapshot the fork/DAG index sizes `(by_hash, dag_heads, author_seqs)`.
    /// Adversarial-suite only.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    #[cfg(feature = "adversarial")]
    pub(crate) async fn index_stats(&self) -> anyhow::Result<(usize, usize, usize)> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.req_tx
            .send(SessionRequest::IndexStats { resp: resp_tx })
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop has stopped"))?;
        resp_rx
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop dropped the response"))
    }

    /// Snapshot the reassembly store's accounting
    /// `(groups, total_bytes, max_author_bytes)`. Adversarial-suite only.
    #[cfg(feature = "adversarial")]
    pub(crate) async fn reassembly_stats(&self) -> anyhow::Result<(usize, usize, usize)> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.req_tx
            .send(SessionRequest::ReassemblyStats { resp: resp_tx })
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop has stopped"))?;
        resp_rx
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop dropped the response"))
    }

    /// Clean shutdown: ask the loop to broadcast `Left` and wind down,
    /// waiting up to 3s. On timeout returns `Ok(())` and `Drop` detaches.
    ///
    /// # Errors
    /// Returns an error if the event-loop task panicked or returned an error.
    pub(crate) async fn leave(mut self) -> anyhow::Result<()> {
        // Stop advertising first — we're leaving, so the listing should age
        // out rather than keep being re-broadcast.
        if let Some(advertiser) = self.advertiser.take() {
            advertiser.abort();
        }
        let _ = self.quit_tx.send(()).await;
        if let Some(task) = self.task.take() {
            let timeout = tokio::time::sleep(Duration::from_secs(3));
            tokio::select! {
                joined = task => {
                    joined
                        .map_err(|error| anyhow::anyhow!("swarm task panicked: {error}"))?
                        .map_err(|error| anyhow::anyhow!("swarm loop error: {error}"))?;
                }
                () = timeout => {}
            }
        }
        Ok(())
    }
}

impl Drop for InProcessSession {
    fn drop(&mut self) {
        // Fallback if `leave()` was never called: abort the loop + advertiser
        // tasks so they don't leak.
        if let Some(task) = self.task.take() {
            task.abort();
        }
        if let Some(advertiser) = self.advertiser.take() {
            advertiser.abort();
        }
    }
}

/// A live swarm membership (the public embed facade): the shared
/// `InProcessSession` plus the inbound broadcast and captured-event
/// stream. Dropping it (or [`SwarmSession::leave`]) winds the loop down.
#[derive(Debug)]
pub struct SwarmSession {
    core: InProcessSession,
    msg_tx: broadcast::Sender<Message>,
    /// Captured structured output events. Single-consumer (mpsc), so
    /// `events()` hands it out exactly once.
    events_rx: Option<mpsc::UnboundedReceiver<OutputEvent>>,
}

impl SwarmSession {
    /// Resolve `cfg.target`, join the swarm, and spawn the event loop in the
    /// background. Output is captured per-session into
    /// [`SwarmSession::events`] (the embedder owns stdout/stderr).
    ///
    /// # Errors
    /// [`JoinError::Resolve`] if the target can't be resolved;
    /// [`JoinError::Setup`] on endpoint/gossip failure.
    pub async fn join(cfg: JoinConfig) -> Result<Self, JoinError> {
        let (output, events_rx) = capture();
        let (elc, io) = join_setup(cfg, output).await?;
        Ok(Self::with_events(elc, io, None, events_rx))
    }

    /// Join a topic — a public swarm derived deterministically from
    /// `cfg.string` — and spawn its event loop in the background. Output is
    /// captured per-session into [`SwarmSession::events`].
    ///
    /// # Errors
    /// [`JoinError::Resolve`] if `cfg.string` is empty/whitespace;
    /// [`JoinError::Setup`] on endpoint/gossip failure.
    pub async fn topic(cfg: TopicConfig) -> Result<Self, JoinError> {
        let (output, events_rx) = capture();
        let (elc, io) = topic_setup(cfg, output).await?;
        Ok(Self::with_events(elc, io, None, events_rx))
    }

    /// Create a new swarm and spawn its event loop in the background.
    /// `cfg.lookups` is resolved the same granular way the CLI uses.
    ///
    /// # Errors
    /// [`CreateError::AdvertiseRequiresReachable`] if `advertise` is set on a
    /// loopback-only swarm; [`CreateError::Setup`] on endpoint/gossip failure.
    pub async fn create(cfg: CreateConfig) -> Result<Self, CreateError> {
        let (output, events_rx) = capture();
        let (elc, io, advertiser) = create_setup(cfg, output).await?;
        Ok(Self::with_events(elc, io, advertiser, events_rx))
    }

    /// Join an already-decoded [`Swarm`] with an explicit co-host policy —
    /// the internal directory-session path (the advertiser eager-cohosts; the
    /// discover consumer never cohosts). `pub(crate)`: keeps `Swarm` off the
    /// iroh-free surface. Directory sessions never register process signal
    /// handlers (see [`DriverMode::InProcess`]) — the hosting command owns
    /// its own lifetime, and hijacking ctrl-c would keep it alive.
    ///
    /// # Errors
    /// Fails if endpoint/gossip setup fails.
    pub(crate) async fn join_decoded(
        swarm: Swarm,
        nickname: Option<Nickname>,
        cohost: CoHostPolicy,
    ) -> anyhow::Result<Self> {
        let author = nickname.unwrap_or_else(Nickname::random);
        let (output, events_rx) = capture();
        let io = crate::a2a::app::SurfacedIo::new(output);
        let sink = io.sink();
        let mut elc = setup_swarm(
            // Directory sessions (advertise/discover) never offload blobs, so no
            // password needs threading here.
            SetupKind::Join {
                swarm,
                password: None,
            },
            SetupParams {
                author,
                interactive: false,
                max_peers: GOSSIP_ACTIVE_VIEW_CAPACITY,
                state_file: None,
                spool: None,
                sink,
                drift: None,
                a2a_serve: None,
                transport: TransportPolicy::default(),
            },
        )
        .await?;
        elc.cohost = cohost;
        Ok(Self::with_events_and_signals(
            elc, io, None, events_rx, false,
        ))
    }

    /// The events presentation: a broadcast for inbound traffic plus the
    /// captured-event stream, over a freshly-spawned [`InProcessSession`]
    /// (which pushes inbound to the broadcast). Registers the process
    /// signal handlers (the public embed default).
    fn with_events(
        elc: EventLoopConfig,
        io: crate::a2a::app::SurfacedIo,
        advertiser: Option<JoinHandle<()>>,
        events_rx: mpsc::UnboundedReceiver<OutputEvent>,
    ) -> Self {
        Self::with_events_and_signals(elc, io, advertiser, events_rx, true)
    }

    /// [`Self::with_events`] with the signal registration explicit — the
    /// directory sessions pass `false`.
    fn with_events_and_signals(
        elc: EventLoopConfig,
        io: crate::a2a::app::SurfacedIo,
        advertiser: Option<JoinHandle<()>>,
        events_rx: mpsc::UnboundedReceiver<OutputEvent>,
        handle_signals: bool,
    ) -> Self {
        let (msg_tx, _initial_rx) = broadcast::channel::<Message>(EMBED_INBOUND_CAP);
        let core =
            InProcessSession::spawn(elc, io, advertiser, Some(msg_tx.clone()), handle_signals);
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

    /// The resolved swarm identifier.
    #[must_use]
    pub fn swarm_id(&self) -> &SwarmId {
        self.core.swarm_id()
    }

    /// The swarm's human-readable name (decoded from the id).
    #[must_use]
    pub fn name(&self) -> &SwarmName {
        self.core.name()
    }

    /// Our effective nickname in this swarm.
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

    /// Build, sign and gossip-broadcast a swarm chat message. Returns the
    /// canonical [`Message`] the loop built (read `.id` for the new id).
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub async fn send(&self, body: MessageBody) -> anyhow::Result<Message> {
        self.core.send(body).await
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
    /// swarm-metadata counterpart of [`state_merge`](Self::state_merge)).
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
    /// alternative to the [`SwarmSession::messages`] live subscription that
    /// surfaces *every* event kind (chat, presence, task legs, and the
    /// transient `ping_report` / `peer_timeout` / … events), each tagged with
    /// its surfacing `seq` — pass the last returned `seq` as the next `after`.
    ///
    /// `long` long-polls: when the buffer is empty, park the read up to the
    /// server cap (~60s), returning early on the first new event — an empty
    /// batch at the deadline just means the window elapsed quietly; call
    /// again. `false` is the immediate read.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub async fn fetch(
        &self,
        after: Option<u64>,
        long: bool,
    ) -> anyhow::Result<Vec<crate::a2a::surfaced::SurfacedEvent>> {
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
    pub async fn task_artifact(
        &self,
        task_id: TaskId,
        text: String,
        file: Option<std::path::PathBuf>,
        file_name: Option<String>,
        file_mime: Option<String>,
    ) -> anyhow::Result<Message> {
        let file = file.map(|path| agent_habilis_gossip::blob::FileRef {
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
    pub async fn a2a_call(
        &self,
        peer: Nickname,
        method: String,
        params: serde_json::Value,
        timeout: Duration,
    ) -> anyhow::Result<serde_json::Value> {
        self.core.a2a_call(peer, method, params, timeout).await
    }

    /// Broadcast pre-built wire bytes **verbatim** into the swarm — no
    /// signing, no chain stamping. Test-only escape hatch (the `adversarial`
    /// feature) for injecting crafted/malicious messages a correct client
    /// would never produce, so the adversarial suite can prove receivers
    /// reject or flag them. Not part of the normal public API.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    #[cfg(feature = "adversarial")]
    pub async fn inject_raw(&self, bytes: Vec<u8>) -> anyhow::Result<()> {
        self.core.inject_raw(bytes::Bytes::from(bytes)).await
    }

    /// This node's iroh endpoint id. Test-only (`adversarial`): a peer needs it
    /// to name this node in an injected circuit topology.
    #[cfg(feature = "adversarial")]
    #[must_use]
    pub fn endpoint_id(&self) -> iroh::EndpointId {
        self.core.endpoint_id()
    }

    /// This node's X25519 circuit key. Test-only (`adversarial`): a peer needs
    /// it to onion-seal a circuit terminating at this node.
    #[cfg(feature = "adversarial")]
    #[must_use]
    pub fn circuit_key(&self) -> [u8; 32] {
        self.core.circuit_key()
    }

    /// Ingest a synthetic link-state vector into this node's routing graph so a
    /// circuit route can be computed over a live mesh that would not otherwise
    /// converge one. Test-only (`adversarial`); mirrors how `src/circuit`'s own
    /// tests build controlled topologies. `origin`/`seal_key`/`links` are a
    /// peer's endpoint id, X25519 key, and `(neighbour, cost)` edges.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    #[cfg(feature = "adversarial")]
    pub async fn inject_link_vector(
        &self,
        origin: iroh::EndpointId,
        seq: u64,
        seal_key: [u8; 32],
        links: Vec<(iroh::EndpointId, u32)>,
    ) -> anyhow::Result<()> {
        self.core
            .inject_link_vector(origin, seq, seal_key, links)
            .await
    }

    /// Simulate the gossip stream terminally ending (the daemon must
    /// resubscribe and recover on its own). Adversarial-suite only.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    #[cfg(feature = "adversarial")]
    pub async fn sever_gossip(&self) -> anyhow::Result<()> {
        self.core.sever_gossip().await
    }

    /// Snapshot the fork/DAG index sizes `(by_hash, dag_heads, author_seqs)`.
    /// Adversarial-suite only — lets it assert that messages we don't
    /// retain are never folded into the indexes (no unbounded leak).
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    #[cfg(feature = "adversarial")]
    pub async fn index_stats(&self) -> anyhow::Result<(usize, usize, usize)> {
        self.core.index_stats().await
    }

    /// Snapshot the reassembly store's accounting
    /// `(groups, total_bytes, max_author_bytes)`. Adversarial-suite only —
    /// lets it assert crafted shard streams stay inside the byte budgets.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    #[cfg(feature = "adversarial")]
    pub async fn reassembly_stats(&self) -> anyhow::Result<(usize, usize, usize)> {
        self.core.reassembly_stats().await
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

pub(crate) use agent_habilis_gossip::daemon::config::DIRECTORY_ADVERTISER_COHOST;

/// Spawn the directory re-broadcast task for `cfg`'s swarm: wire a fresh
/// live-participant counter into `cfg.live_count`, then re-send the
/// swarm's `💬…` id (with that count) into `directory` every
/// `ADVERTISE_INTERVAL_SECS` over the swarm's own `lookups`. Returns the
/// task handle so the owner can abort it (the inner directory session is
/// dropped with the task, closing that membership). A directory-join
/// failure logs and ends the task — the swarm is unaffected, just unlisted.
pub(crate) fn spawn_advertiser(
    cfg: &mut EventLoopConfig,
    directory: SwarmName,
    lookups: LookupOpts,
) -> JoinHandle<()> {
    let live_count = Arc::new(AtomicUsize::new(1));
    cfg.live_count = Some(live_count.clone());
    let swarm_id = cfg.swarm.clone();
    tokio::spawn(async move {
        // Reach the directory over the advertised swarm's own lookups, so an
        // mDNS-only swarm advertises over mDNS only (no DHT/relay touched) and
        // discoverers must use the same lookups to meet.
        let swarm = directory_swarm(&directory, lookups);
        // Co-host the directory rendezvous from t=0 so a beacon exists
        // before any discoverer subscribes (a `Deferred` advertiser would
        // only beacon at the first heal tick, after discoverers already
        // failed their first graft). Probe-first; see DIRECTORY_ADVERTISER_COHOST.
        let session =
            match SwarmSession::join_decoded(swarm, None, DIRECTORY_ADVERTISER_COHOST).await {
                Ok(session) => session,
                Err(error) => {
                    tracing::warn!(
                        target: "agent_gossip::directory",
                        %error,
                        directory = %directory,
                        "directory advertise: could not join the directory; swarm stays unlisted"
                    );
                    return;
                }
            };
        let mut ticker = tokio::time::interval(Duration::from_secs(advertise_interval_secs()));
        loop {
            ticker.tick().await;
            let ad = directory::Ad {
                id: swarm_id.clone(),
                peers: live_count.load(Ordering::Relaxed),
            };
            if let Err(error) = session.send(ad.to_body()).await {
                tracing::debug!(
                    target: "agent_gossip::directory",
                    %error,
                    "directory advertise: re-broadcast failed (will retry next tick)"
                );
            }
        }
    })
}

// ── Directory (directory consumer) ─────────────────────────────────────

/// One live directory entry handed to embedders — the public, iroh-free
/// projection of a `agent_habilis_gossip::directory::Listing`.
#[derive(Debug, Clone)]
pub struct SwarmListing {
    /// The advertised swarm's id — pass to [`SwarmSession::join`] to join.
    pub swarm: SwarmId,
    /// Human-readable swarm name (decoded from the id).
    pub name: SwarmName,
    /// `true` if the swarm is on the public network.
    pub public: bool,
    /// `true` if the swarm id carries a password verifier — joining needs
    /// the password, so the listing alone does not admit.
    pub password: bool,
    /// Live participant count from the most recent ad.
    pub peers: usize,
    /// Unix seconds when this swarm was first seen in the directory
    /// (stable across re-ads).
    pub first_seen_unix: i64,
}

/// A directory change observed by a [`Directory`].
#[derive(Debug, Clone)]
pub enum DirectoryEvent {
    /// A swarm appeared in the directory.
    Found(SwarmListing),
    /// An already-listed swarm re-advertised (refreshed count/freshness).
    Updated(SwarmListing),
    /// A swarm's ads stopped and its listing aged out.
    Lost(SwarmId),
}

fn public_listing(listing: &Listing) -> SwarmListing {
    SwarmListing {
        swarm: listing.swarm.clone(),
        name: listing.name.clone(),
        public: listing.public,
        password: listing.password,
        peers: listing.peers,
        first_seen_unix: listing.first_seen_unix,
    }
}

/// A live view of a directory. Joins the directory's swarm as an
/// ordinary [`SwarmSession`] and collects advertisements into
/// `Listings`, aging out swarms whose publishers went silent. Drop
/// (or let it fall out of scope) to leave the directory.
///
/// ```no_run
/// # use agent_gossip::embed::Directory;
/// # use agent_gossip::LookupSet;
/// # async fn run() -> anyhow::Result<()> {
/// // Bare `LookupSet::default()` ⇒ all-on (mDNS + DHT + relay).
/// let mut directory = Directory::open(Some("demo"), LookupSet::default()).await?;
/// for listing in directory.snapshot() {
///     println!("#{} — {} peers — {}", listing.name, listing.peers, listing.swarm.as_str());
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct Directory {
    session: Option<SwarmSession>,
    listings: Arc<Mutex<Listings>>,
    events_rx: Option<mpsc::UnboundedReceiver<DirectoryEvent>>,
    task: Option<JoinHandle<()>>,
}

impl Directory {
    /// Open a directory by name, reaching it over `lookups`. `name` is the
    /// directory name; `None` ⇒ the well-known `global` directory. The
    /// directory's topic is keyed by name **and** the lookups in use, so a
    /// discoverer only sees advertisers that reached the directory over the
    /// **same** lookups; bare `LookupSet::default()` resolves to the all-on
    /// preset (mDNS + DHT + relay), matching a `--public` advertiser. A
    /// disabled leg issues no network requests for the directory. Returns once
    /// the directory session is ready; listings then accumulate in the
    /// background.
    ///
    /// # Errors
    /// Fails if the name is invalid or the directory session cannot be
    /// established (endpoint/gossip setup, bootstrap unreachable).
    ///
    /// # Panics
    /// Panics only if the internal collector mutex is poisoned by a
    /// panic in the background task — not reachable in normal use.
    pub async fn open(name: Option<impl Into<String>>, lookups: LookupSet) -> anyhow::Result<Self> {
        let directory_name = match name {
            Some(value) => SwarmName::new(value.into())
                .map_err(|error| anyhow::anyhow!("invalid directory name: {error}"))?,
            None => {
                SwarmName::new(DEFAULT_DIRECTORY).expect("DEFAULT_DIRECTORY is a valid swarm name")
            }
        };
        // Directories are inherently networked, so resolve as if `--public`:
        // no flags ⇒ all-on. The test env forces loopback so the hermetic
        // advertise→discover path runs without the public relay.
        let resolved = resolve_lookups(
            !agent_habilis_gossip::util::tuning::directory_private_for_test(),
            lookups,
        );
        let swarm = directory_swarm(&directory_name, resolved);
        // A discoverer is a pure consumer: never co-host the directory's
        // rendezvous (it only dials an advertiser's beacon).
        let session = SwarmSession::join_decoded(swarm, None, CoHostPolicy::Never).await?;
        let mut inbound = session.messages();
        let listings = Arc::new(Mutex::new(Listings::new()));
        let (events_tx, events_rx) = mpsc::unbounded_channel();

        let collector = listings.clone();
        let task = tokio::spawn(async move {
            let ttl = Duration::from_secs(directory_expiry_secs());
            let mut expiry = tokio::time::interval(ttl);
            expiry.tick().await; // eat the immediate first tick
            loop {
                tokio::select! {
                    received = inbound.recv() => match received {
                        Ok(message) => {
                            let now = Instant::now();
                            let event = {
                                let mut dir = collector.lock().expect("directory mutex not poisoned");
                                // Ads ride broadcast chat frames, so the ad
                                // text is the payload's text projection, not
                                // the raw frame body (a serialized A2A object).
                                let Some(ad_text) = crate::a2a::gossip::chat_text(&message) else {
                                    continue;
                                };
                                match dir.note(&ad_text, now) {
                                    Some(ListingChange::Found(id)) => dir
                                        .get(&id)
                                        .map(|listing| DirectoryEvent::Found(public_listing(listing))),
                                    Some(ListingChange::Updated(id)) => dir
                                        .get(&id)
                                        .map(|listing| DirectoryEvent::Updated(public_listing(listing))),
                                    None => None,
                                }
                            };
                            if let Some(event) = event {
                                let _ = events_tx.send(event);
                            }
                        }
                        // Slow consumer dropped some inbound — listings
                        // self-heal on the next re-ad, so skip and continue.
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                    _ = expiry.tick() => {
                        let now = Instant::now();
                        let lost = {
                            let mut dir = collector.lock().expect("directory mutex not poisoned");
                            dir.expire(ttl, now)
                        };
                        for id in lost {
                            let _ = events_tx.send(DirectoryEvent::Lost(id));
                        }
                    }
                }
            }
        });

        Ok(Self {
            session: Some(session),
            listings,
            events_rx: Some(events_rx),
            task: Some(task),
        })
    }

    /// The current live listings, sorted by name then id.
    ///
    /// # Panics
    /// Panics only if the internal collector mutex was poisoned by a
    /// prior panic in the background task — not reachable in normal use.
    #[must_use]
    pub fn snapshot(&self) -> Vec<SwarmListing> {
        self.listings
            .lock()
            .expect("directory mutex not poisoned")
            .snapshot()
            .iter()
            .map(public_listing)
            .collect()
    }

    /// Take the directory event stream (`Found` / `Updated` / `Lost`).
    /// Single-consumer, so this returns the receiver **once**;
    /// subsequent calls return `None`.
    pub fn events(&mut self) -> Option<mpsc::UnboundedReceiver<DirectoryEvent>> {
        self.events_rx.take()
    }

    /// The directory session's `(swarm id, nickname)` while it is open —
    /// used by the CLI to route its logs to the per-member file via
    /// `logging::attach` (so the picker / JSON stream stays clean).
    pub(crate) fn session_identity(&self) -> Option<(&SwarmId, &Nickname)> {
        self.session
            .as_ref()
            .map(|session| (session.swarm_id(), session.nickname()))
    }

    /// Leave the directory and stop collecting.
    ///
    /// # Errors
    /// Propagates a clean-shutdown error from the underlying directory
    /// [`SwarmSession::leave`] (event-loop task panic / loop error).
    pub async fn close(mut self) -> anyhow::Result<()> {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        if let Some(session) = self.session.take() {
            session.leave().await?;
        }
        Ok(())
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        // The directory `SwarmSession` (if not already taken by `close`)
        // drops here, winding down its own loop.
    }
}

#[cfg(test)]
mod topic_tests {
    use std::time::Duration;

    use agent_habilis_gossip::daemon::CoHostPolicy;
    use agent_habilis_gossip::protocol::swarm::{Swarm, SwarmConfig};
    use agent_habilis_gossip::protocol::{MessageBody, Nickname};

    use super::{JoinError, SwarmSession, TopicConfig};

    /// The empty/whitespace-string guard is centralized in `TopicParams::resolve`,
    /// so it holds for the public embed API too (not just the CLI/MCP edges) —
    /// an empty string would otherwise join one globally-fixed swarm.
    #[tokio::test]
    async fn topic_rejects_empty_string() {
        for raw in ["", "   "] {
            let result = SwarmSession::topic(TopicConfig::new(raw.to_owned())).await;
            assert!(
                matches!(result, Err(JoinError::Resolve(_))),
                "empty topic string must be rejected, got {:?}",
                result.map(|_| "Ok")
            );
        }
    }

    /// End-to-end convergence: two peers deriving from the *same string* land
    /// in the same swarm and mesh. Loopback keeps the test hermetic; the seed +
    /// name derivation and the `EagerProbed` first-peer beaconing are identical
    /// to a real (public) `topic` — only the transport differs.
    #[tokio::test]
    async fn topic_peers_from_same_string_converge_and_exchange() {
        let topic = "agent-habilis";
        let first = Swarm::from_topic(topic, SwarmConfig::loopback());
        let second = Swarm::from_topic(topic, SwarmConfig::loopback());
        assert_eq!(
            first.to_string(),
            second.to_string(),
            "same string ⇒ same swarm id"
        );

        let alice = SwarmSession::join_decoded(
            first,
            Some(Nickname::new("alice-topic").unwrap()),
            CoHostPolicy::EagerProbed,
        )
        .await
        .expect("alice topic session");
        let bob = SwarmSession::join_decoded(
            second,
            Some(Nickname::new("bob-topic").unwrap()),
            CoHostPolicy::EagerProbed,
        )
        .await
        .expect("bob topic session");
        assert_eq!(alice.swarm_id(), bob.swarm_id(), "both derived the same id");

        let mut bob_rx = bob.messages();
        // Re-send until the loopback mesh forms; break the instant bob sees it.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let mut received = false;
        while !received && tokio::time::Instant::now() < deadline {
            alice
                .send(MessageBody::from("hello topic"))
                .await
                .expect("alice send");
            let seen = tokio::time::timeout(Duration::from_millis(500), async {
                loop {
                    match bob_rx.recv().await {
                        Ok(msg) => {
                            if msg.author.as_str() == "alice-topic"
                                && crate::a2a::gossip::chat_text(&msg).as_deref()
                                    == Some("hello topic")
                            {
                                break true;
                            }
                        }
                        Err(_) => break false,
                    }
                }
            })
            .await;
            received = matches!(seen, Ok(true));
        }
        assert!(
            received,
            "bob should receive alice's message over the topic swarm"
        );

        bob.leave().await.ok();
        alice.leave().await.ok();
    }
}
