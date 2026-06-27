//! In-process embedding facade.
//!
//! [`SwarmSession`] runs the swarm event loop as a background `tokio`
//! task **inside the caller's process** — no subprocess, no Unix-socket
//! IPC. Inbound traffic is pushed over a bounded broadcast channel;
//! outbound sends go through a dedicated channel into the same shared
//! broadcast path the CLI/IPC uses. No `iroh` type is exposed: targets
//! are resolved internally from a string (`🐝…` / domain / git URL).

use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::daemon::setup::{SetupKind, setup_swarm};
use crate::daemon::state::RosterSnapshot;
use crate::daemon::{
    CoHostPolicy, CreateParams, DriverMode, EventLoopConfig, JoinParams, Resolved, SessionRequest,
};
use crate::directory::{self, Listing, ListingChange, Listings, directory_swarm};
use crate::output::{Output, OutputEvent};
use crate::protocol::swarm::{
    DEFAULT_DIRECTORY, DirectorySelection, LookupOpts, LookupSet, Swarm, SwarmConfig, SwarmName,
    resolve_lookups,
};
use crate::protocol::{
    ExchangeId, ExchangeKind, ExchangePhase, Message, MessageBody, Nickname, SwarmId,
};
use crate::resolver::JoinTarget;
use crate::util::tuning::{
    DEFAULT_MAX_DIRECT_PEERS, EMBED_INBOUND_CAP, advertise_interval_secs, directory_expiry_secs,
};

/// How to join a swarm.
#[derive(Debug, Clone)]
pub struct JoinConfig {
    /// What to join: an `🐝…` id, a domain serving
    /// `/.well-known/agent-habilis-swarm`, or a supported git repo URL —
    /// classified into a [`JoinTarget`] at the boundary (parse a string
    /// with [`str::parse`]). Resolved internally; the network mode and
    /// name are decoded from the resolved swarm.
    pub target: JoinTarget,
    /// Local nickname. `None` mints a random `word-word` one.
    pub nickname: Option<Nickname>,
    /// Max direct peer connections before gossip relays the rest.
    pub max_peers: usize,
    /// Self-reported model (e.g. `"Opus 4.8"`), announced to peers so the
    /// roster shows what this agent runs on. `None` ⇒ advertise nothing.
    pub model: Option<String>,
    /// Self-reported harness (e.g. `"Claude Code"`), announced alongside
    /// `model`. `None` ⇒ advertise nothing.
    pub harness: Option<String>,
}

impl JoinConfig {
    /// A config for `target` with a random nickname and the default
    /// peer cap. Set [`JoinConfig::nickname`] / [`JoinConfig::max_peers`]
    /// afterwards to override. Build the [`JoinTarget`] by parsing a
    /// string (`"🐝…".parse()?`).
    #[must_use]
    pub fn new(target: JoinTarget) -> Self {
        Self {
            target,
            nickname: None,
            max_peers: DEFAULT_MAX_DIRECT_PEERS,
            model: None,
            harness: None,
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
    /// without its `🐝…` id. Requires `public`. Default `false`.
    pub advertise: bool,
    /// The directory to advertise into when `advertise` is set.
    /// `None` ⇒ the well-known `global` directory.
    pub directory: Option<SwarmName>,
    /// Per-author messages-per-minute cap baked into the swarm id and
    /// enforced swarm-wide. `0` disables rate limiting. Default 60.
    pub rate_limit_per_min: u16,
    /// Max direct peer connections before gossip relays the rest.
    pub max_peers: usize,
    /// Self-reported model (e.g. `"Opus 4.8"`), announced to peers so the
    /// roster shows what this agent runs on. `None` ⇒ advertise nothing.
    pub model: Option<String>,
    /// Self-reported harness (e.g. `"Claude Code"`), announced alongside
    /// `model`. `None` ⇒ advertise nothing.
    pub harness: Option<String>,
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
            rate_limit_per_min: crate::util::consts::RATE_LIMIT_PER_MIN,
            max_peers: DEFAULT_MAX_DIRECT_PEERS,
            model: None,
            harness: None,
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
                    crate::protocol::swarm::AdvertiseRequiresReachable
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
/// [`CreateError`]. `Resolve` is a bad target (an `🐝…` id, a domain, or a
/// git-repo URL and its well-known file); `Setup` is an endpoint/gossip
/// failure. The MCP server maps both to an internal error.
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
) -> Result<(EventLoopConfig, Option<JoinHandle<()>>), CreateError> {
    let config = SwarmConfig {
        rate_limit_per_min: cfg.rate_limit_per_min,
        lookups: resolve_lookups(cfg.public, cfg.lookups),
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
    }
    .resolve()
    .map_err(|_| CreateError::AdvertiseRequiresReachable)?;
    let mut elc = setup_swarm(
        kind, author, /* interactive */ false, max_peers, None, output, /* drift */ None,
    )
    .await
    .map_err(|error| CreateError::Setup(error.context("setup_swarm failed")))?;
    // Self-reported identity, announced in our `joined` body — same
    // late-assignment the CLI uses (keeps `setup_swarm`'s arg count in budget).
    elc.model = cfg.model;
    elc.harness = cfg.harness;
    // When advertising, start the re-broadcast task (tied to this session);
    // it reaches the directory over this swarm's own lookups (moved into the
    // at-most-once closure, so no clone).
    let advertiser = advertise_directory
        .map(|directory| spawn_advertiser(&mut elc, directory, directory_lookups));
    Ok((elc, advertiser))
}

/// Resolve + set up a join: the ready [`EventLoopConfig`]. The caller picks
/// the `output` sink.
///
/// # Errors
/// [`JoinError::Resolve`] / [`JoinError::Setup`].
async fn join_setup(cfg: JoinConfig, output: Output) -> Result<EventLoopConfig, JoinError> {
    let max_peers = cfg.max_peers;
    let Resolved { kind, author, .. } = JoinParams {
        target: cfg.target,
        nickname: cfg.nickname,
    }
    .resolve()
    .await
    .map_err(JoinError::Resolve)?;
    let mut elc = setup_swarm(
        kind, author, /* interactive */ false, max_peers, None, output, /* drift */ None,
    )
    .await
    .map_err(|error| JoinError::Setup(error.context("setup_swarm failed")))?;
    // Self-reported identity, announced in our `joined` body (see `create_setup`).
    elc.model = cfg.model;
    elc.harness = cfg.harness;
    Ok(elc)
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
    fn spawn(
        mut elc: EventLoopConfig,
        advertiser: Option<JoinHandle<()>>,
        push: Option<broadcast::Sender<Message>>,
    ) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<SessionRequest>(32);
        let (quit_tx, quit_rx) = mpsc::channel::<()>(1);
        elc.driver = DriverMode::InProcess {
            msg_tx: push,
            req_rx,
            quit_rx,
        };
        let swarm_id = elc.swarm.clone();
        let name = elc.name.clone();
        let nickname = elc.author.clone();
        let task = tokio::spawn(crate::daemon::run(elc));
        Self {
            swarm_id,
            name,
            nickname,
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
        let (elc, advertiser) = create_setup(cfg, Output::silent()).await?;
        Ok(Self::spawn(elc, advertiser, None))
    }

    /// Join an existing swarm as a poll-only, silent core (the MCP server).
    ///
    /// # Errors
    /// [`JoinError::Resolve`] / [`JoinError::Setup`].
    pub(crate) async fn join_poll(cfg: JoinConfig) -> Result<Self, JoinError> {
        let elc = join_setup(cfg, Output::silent()).await?;
        Ok(Self::spawn(elc, None, None))
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
    pub(crate) async fn send(
        &self,
        body: MessageBody,
        reply: Option<Nickname>,
    ) -> anyhow::Result<Option<Message>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.req_tx
            .send(SessionRequest::Send {
                body,
                reply,
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
        wait_ms: Option<u64>,
    ) -> anyhow::Result<Vec<crate::daemon::surfaced::SurfacedEvent>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.req_tx
            .send(SessionRequest::Poll {
                after,
                wait_ms,
                resp: resp_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop has stopped"))?;
        resp_rx
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop dropped the response"))
    }

    /// Send one leg of an exchange; returns the canonical
    /// [`Message`] or `None` when the sender-side rate limiter dropped it.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub(crate) async fn exchange(
        &self,
        to: Nickname,
        exchange_id: ExchangeId,
        kind: ExchangeKind,
        phase: ExchangePhase,
        body: MessageBody,
    ) -> anyhow::Result<Option<Message>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.req_tx
            .send(SessionRequest::Exchange {
                to,
                exchange_id,
                kind,
                phase,
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
        resp_rx
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop dropped the response"))
    }

    /// Author + broadcast a durable state event (the write primitive for
    /// state-backed features). Retained locally and gossiped to peers.
    pub(crate) async fn append_state(&self, body: MessageBody) -> anyhow::Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.req_tx
            .send(SessionRequest::AppendState {
                body,
                resp: resp_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop has stopped"))?;
        resp_rx
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop dropped the response"))?
    }

    /// The derived swarm state — event payloads in deterministic replay order.
    pub(crate) async fn state_snapshot(&self) -> anyhow::Result<Vec<String>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.req_tx
            .send(SessionRequest::StateSnapshot { resp: resp_tx })
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop has stopped"))?;
        resp_rx
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop dropped the response"))
    }

    /// Apply a JSON-Patch change to the shared state. The op array (frozen
    /// subset) is validated against the current document and rate-limited; a
    /// rejected patch returns an error.
    pub(crate) async fn apply_patch(&self, patch: serde_json::Value) -> anyhow::Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.req_tx
            .send(SessionRequest::StatePatch {
                patch,
                resp: resp_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop has stopped"))?;
        resp_rx
            .await
            .map_err(|_| anyhow::anyhow!("swarm event loop dropped the response"))?
    }

    /// The current derived shared-state document (the JSON-Patch fold).
    pub(crate) async fn state_document(&self) -> anyhow::Result<serde_json::Value> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.req_tx
            .send(SessionRequest::StateDocument { resp: resp_tx })
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
        let elc = join_setup(cfg, output).await?;
        Ok(Self::with_events(elc, None, events_rx))
    }

    /// Create a new swarm and spawn its event loop in the background.
    /// `cfg.lookups` is resolved the same granular way the CLI uses.
    ///
    /// # Errors
    /// [`CreateError::AdvertiseRequiresReachable`] if `advertise` is set on a
    /// loopback-only swarm; [`CreateError::Setup`] on endpoint/gossip failure.
    pub async fn create(cfg: CreateConfig) -> Result<Self, CreateError> {
        let (output, events_rx) = capture();
        let (elc, advertiser) = create_setup(cfg, output).await?;
        Ok(Self::with_events(elc, advertiser, events_rx))
    }

    /// Join an already-decoded [`Swarm`] with an explicit co-host policy —
    /// the internal directory-session path (the advertiser eager-cohosts; the
    /// discover consumer never cohosts). `pub(crate)`: keeps `Swarm` off the
    /// iroh-free surface.
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
        let mut elc = setup_swarm(
            SetupKind::Join { swarm },
            author,
            /* interactive */ false,
            DEFAULT_MAX_DIRECT_PEERS,
            /* state_file */ None,
            output,
            /* drift */ None,
        )
        .await?;
        elc.cohost = cohost;
        Ok(Self::with_events(elc, None, events_rx))
    }

    /// The events presentation: a broadcast for inbound traffic plus the
    /// captured-event stream, over a freshly-spawned [`InProcessSession`]
    /// (which pushes inbound to the broadcast).
    fn with_events(
        elc: EventLoopConfig,
        advertiser: Option<JoinHandle<()>>,
        events_rx: mpsc::UnboundedReceiver<OutputEvent>,
    ) -> Self {
        let (msg_tx, _initial_rx) = broadcast::channel::<Message>(EMBED_INBOUND_CAP);
        let core = InProcessSession::spawn(elc, advertiser, Some(msg_tx.clone()));
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

    /// Build, sign and gossip-broadcast a message. Returns the canonical
    /// [`Message`] the loop built (read `.id` for the new id), or `None` when
    /// the sender-side rate limiter dropped it.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub async fn send(
        &self,
        body: MessageBody,
        reply: Option<Nickname>,
    ) -> anyhow::Result<Option<Message>> {
        self.core.send(body, reply).await
    }

    /// Append a durable state event (the write primitive future state-backed
    /// features build on): it is retained in this node's un-pruned state log
    /// and gossiped to peers, who reconcile it via state anti-entropy.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or the broadcast failed.
    pub async fn append_state(&self, body: MessageBody) -> anyhow::Result<()> {
        self.core.append_state(body).await
    }

    /// The derived swarm state — event payloads in deterministic replay order
    /// (the substrate's generic read until typed projections land).
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub async fn state_snapshot(&self) -> anyhow::Result<Vec<String>> {
        self.core.state_snapshot().await
    }

    /// Apply a JSON-Patch change to the shared state (frozen subset:
    /// add/replace/remove on object paths + add `/arr/-`). Validated against the
    /// current document and rate-limited.
    ///
    /// # Errors
    /// Fails if the patch is out of subset / does not apply, the rate limit is
    /// exceeded, or the event loop has stopped.
    pub async fn apply_patch(&self, patch: serde_json::Value) -> anyhow::Result<()> {
        self.core.apply_patch(patch).await
    }

    /// The current derived shared-state document (the JSON-Patch fold over the
    /// state log).
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub async fn state_document(&self) -> anyhow::Result<serde_json::Value> {
        self.core.state_document().await
    }

    /// Poll the surfaced-event history after the `after` seq cursor (`None`
    /// for the full buffered window). Join-horizon filtered. A pull
    /// alternative to the [`SwarmSession::messages`] live subscription that
    /// surfaces *every* event kind (chat, presence, exchange legs, and the
    /// transient `ping_report` / `peer_timeout` / … events), each tagged with
    /// its surfacing `seq` — pass the last returned `seq` as the next `after`.
    ///
    /// `wait_ms` long-polls: when the buffer is empty, block up to that many
    /// ms (server-clamped) for a new event before returning, instead of an
    /// immediate empty read. `None`/`0` is the immediate read.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub async fn fetch(
        &self,
        after: Option<u64>,
        wait_ms: Option<u64>,
    ) -> anyhow::Result<Vec<crate::daemon::surfaced::SurfacedEvent>> {
        self.core.fetch(after, wait_ms).await
    }

    /// Send one leg of an exchange to `to`, correlated by `exchange_id`.
    /// Returns the canonical [`Message`] the loop built, or `None` when the
    /// sender-side rate limiter dropped it. Addressee validation (for
    /// `Offer`) happens in `broadcast_exchange` — see the MCP `send_exchange` tool.
    ///
    /// # Errors
    /// Fails if the event loop has stopped or dropped the response.
    pub async fn exchange(
        &self,
        to: Nickname,
        exchange_id: ExchangeId,
        kind: ExchangeKind,
        phase: ExchangePhase,
        body: MessageBody,
    ) -> anyhow::Result<Option<Message>> {
        self.core.exchange(to, exchange_id, kind, phase, body).await
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

    /// Clean shutdown: ask the loop to broadcast `Left` and wind down,
    /// waiting up to 3s. On timeout returns `Ok(())` and the task detaches.
    ///
    /// # Errors
    /// Returns an error if the event-loop task panicked or returned an error.
    pub async fn leave(self) -> anyhow::Result<()> {
        self.core.leave().await
    }
}

/// The co-host policy for a directory advertiser: claim the directory's
/// shared rendezvous from t=0 but probe-first. See
/// [`CoHostPolicy::EagerProbed`] for why probing matters here.
pub(crate) const DIRECTORY_ADVERTISER_COHOST: CoHostPolicy = CoHostPolicy::EagerProbed;

/// Spawn the directory re-broadcast task for `cfg`'s swarm: wire a fresh
/// live-participant counter into `cfg.live_count`, then re-send the
/// swarm's `🐝…` id (with that count) into `directory` every
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
                        target: "agent_habilis_swarm::directory",
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
            if let Err(error) = session.send(ad.to_body(), None).await {
                tracing::debug!(
                    target: "agent_habilis_swarm::directory",
                    %error,
                    "directory advertise: re-broadcast failed (will retry next tick)"
                );
            }
        }
    })
}

// ── Directory (directory consumer) ─────────────────────────────────────

/// One live directory entry handed to embedders — the public, iroh-free
/// projection of a `crate::directory::Listing`.
#[derive(Debug, Clone)]
pub struct SwarmListing {
    /// The advertised swarm's id — pass to [`SwarmSession::join`] to join.
    pub swarm: SwarmId,
    /// Human-readable swarm name (decoded from the id).
    pub name: SwarmName,
    /// `true` if the swarm is on the public network.
    pub public: bool,
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
/// # use agent_habilis_swarm::embed::Directory;
/// # use agent_habilis_swarm::LookupSet;
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
        let resolved = resolve_lookups(!crate::util::tuning::directory_private_for_test(), lookups);
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
                                match dir.note(message.body.as_str(), now) {
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
