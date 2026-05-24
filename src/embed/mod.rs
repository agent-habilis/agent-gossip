//! In-process embedding facade.
//!
//! [`SwarmSession`] runs the swarm event loop as a background `tokio`
//! task **inside the caller's process** — no subprocess, no Unix-socket
//! IPC. Inbound traffic is pushed over a bounded broadcast channel;
//! outbound sends go through a dedicated channel into the same shared
//! broadcast path the CLI/IPC uses. No `iroh` type is exposed: targets
//! are resolved internally from a string (`ahs…` / domain / git URL).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::daemon::setup::{SetupKind, setup_swarm};
use crate::daemon::{CoHostPolicy, DriverMode, EventLoopConfig, SendRequest};
use crate::directory::{self, Listing, ListingChange, Listings, directory_swarm};
use crate::output::{Output, OutputEvent};
use crate::protocol::swarm::{
    DEFAULT_DIRECTORY, LookupOpts, Swarm, SwarmMode, SwarmName, resolve_relay,
};
use crate::protocol::{Message, MessageBody, MessageId, Nickname, SwarmId};
use crate::resolver;
use crate::util::tuning::{
    DEFAULT_MAX_DIRECT_PEERS, EMBED_INBOUND_CAP, advertise_interval_secs, directory_expiry_secs,
};

/// How to join a swarm.
#[derive(Debug, Clone)]
pub struct JoinConfig {
    /// What to join: an `ahs…` id, a domain serving
    /// `/.well-known/agent-habilis-swarm`, or a supported git repo URL.
    /// Resolved internally; the network mode and name are decoded from
    /// the resolved swarm.
    pub target: String,
    /// Local nickname. `None` mints a random `word-word` one.
    pub nickname: Option<Nickname>,
    /// Max direct peer connections before gossip relays the rest.
    pub max_peers: usize,
}

impl JoinConfig {
    /// A config for `target` with a random nickname and the default
    /// peer cap. Set [`JoinConfig::nickname`] / [`JoinConfig::max_peers`]
    /// afterwards to override.
    #[must_use]
    pub fn new(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            nickname: None,
            max_peers: DEFAULT_MAX_DIRECT_PEERS,
        }
    }
}

/// How to create a new swarm. String/primitive-typed to keep the
/// embed surface iroh-free (mirrors [`JoinConfig`]); `name`/`relay`
/// are validated/parsed when the session is created.
#[derive(Debug, Clone)]
pub struct CreateConfig {
    /// 1..=32 UTF-8 characters (any script/emoji), excluding control
    /// characters, whitespace, and any of `/ \ < > #`.
    pub name: String,
    /// Local nickname. `None` mints a random `word-word` one.
    pub nickname: Option<Nickname>,
    /// `true` = public (cross-machine networking, pkarr lookup); `false`
    /// = private (localhost only). Default `false`.
    pub public: bool,
    /// Custom relay URL, honored only with `public`. `None` uses the
    /// default relay. Parsed internally.
    pub relay: Option<String>,
    /// List this swarm in a directory so discoverers can find it
    /// without its `ahs…` id. Requires `public`. Default `false`.
    pub advertise: bool,
    /// The directory to advertise into when `advertise` is set.
    /// `None` ⇒ the well-known `global` directory.
    pub directory: Option<String>,
    /// Max direct peer connections before gossip relays the rest.
    pub max_peers: usize,
}

impl CreateConfig {
    /// A private-network config for swarm `name` with a random
    /// nickname and the default peer cap. Set the other fields
    /// afterwards to override.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            nickname: None,
            public: false,
            relay: None,
            advertise: false,
            directory: None,
            max_peers: DEFAULT_MAX_DIRECT_PEERS,
        }
    }
}

/// A live swarm membership. The event loop runs as a background task
/// for the lifetime of this value; dropping it (or calling
/// [`SwarmSession::leave`]) winds the loop down.
#[derive(Debug)]
pub struct SwarmSession {
    swarm_id: SwarmId,
    nickname: Nickname,
    msg_tx: broadcast::Sender<Message>,
    send_tx: mpsc::Sender<SendRequest>,
    quit_tx: mpsc::Sender<()>,
    /// Captured structured output events. Single-consumer (mpsc), so
    /// `events()` hands it out exactly once.
    events_rx: Option<mpsc::UnboundedReceiver<OutputEvent>>,
    task: Option<JoinHandle<anyhow::Result<()>>>,
    /// The directory re-broadcast task, when this session was created
    /// with `advertise`. Tied to the session's lifetime — aborted on
    /// `leave`/drop so we don't keep advertising a swarm we left.
    advertiser: Option<JoinHandle<()>>,
}

impl SwarmSession {
    /// Resolve `cfg.target`, join the swarm, and spawn the event loop
    /// in the background. Returns once the session is ready — the
    /// resolved [`SwarmSession::swarm_id`] and effective
    /// [`SwarmSession::nickname`] are known.
    ///
    /// Output is captured per-session into [`SwarmSession::events`]
    /// (the embedder owns stdout/stderr; nothing is printed). Unlike
    /// the old process-global switch, each session has an independent
    /// sink, so multiple in-process sessions don't interfere.
    ///
    /// # Errors
    /// Fails if the target cannot be resolved, the endpoint/gossip
    /// setup fails, or the join times out (bootstrap peer unreachable).
    pub async fn join(cfg: JoinConfig) -> anyhow::Result<Self> {
        let swarm = resolver::resolve(&cfg.target).await?;
        let lookups = LookupOpts::default_for(swarm.mode, None);
        let author = cfg.nickname.unwrap_or_else(Nickname::random);
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let elc = setup_swarm(
            SetupKind::Join { swarm },
            author,
            /* interactive */ false,
            cfg.max_peers,
            /* state_file */ None,
            lookups,
            Output::capture(events_tx),
        )
        .await?;
        Ok(Self::spawn_session_from(elc, events_rx, None))
    }

    /// Join an already-decoded [`Swarm`] with an explicit lookup set and
    /// co-host policy — the internal path for directory sessions.
    /// Unlike [`SwarmSession::join`], it skips the string resolve and
    /// the default lookups, and lets the caller pick the beacon role:
    /// the advertiser passes [`CoHostPolicy::Eager`] (be the directory's
    /// beacon from t=0), the [`Directory`] consumer passes
    /// [`CoHostPolicy::Never`] (it only dials an existing beacon).
    /// `pub(crate)`: keeps `Swarm`/`LookupOpts` off the iroh-free surface.
    ///
    /// # Errors
    /// Fails if endpoint/gossip setup fails or the join times out.
    pub(crate) async fn join_with_lookups(
        swarm: Swarm,
        lookups: LookupOpts,
        nickname: Option<Nickname>,
        cohost: CoHostPolicy,
    ) -> anyhow::Result<Self> {
        let author = nickname.unwrap_or_else(Nickname::random);
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let mut elc = setup_swarm(
            SetupKind::Join { swarm },
            author,
            /* interactive */ false,
            DEFAULT_MAX_DIRECT_PEERS,
            /* state_file */ None,
            lookups,
            Output::capture(events_tx),
        )
        .await?;
        elc.cohost = cohost;
        Ok(Self::spawn_session_from(elc, events_rx, None))
    }

    /// Create a new swarm and spawn its event loop in the background.
    /// `cfg.name` is validated and `cfg.relay` parsed here, keeping the
    /// public surface iroh-free.
    ///
    /// # Errors
    /// Fails if the name is invalid, a relay is given without
    /// `public`, the relay URL is unparseable, or endpoint/gossip
    /// setup fails.
    pub async fn create(cfg: CreateConfig) -> anyhow::Result<Self> {
        let name = SwarmName::new(cfg.name)
            .map_err(|error| anyhow::anyhow!("invalid swarm name: {error:?}"))?;
        let mode = if cfg.public {
            SwarmMode::Public
        } else {
            SwarmMode::Private
        };
        let relay = resolve_relay(mode, cfg.relay.as_deref())?;
        let lookups = LookupOpts::default_for(mode, relay);
        // Resolve the advertise directory up front so an invalid directory /
        // private-mode advertise fails before the session is spawned.
        let directory = resolve_advertise_directory(cfg.advertise, cfg.directory, mode)?;
        let author = cfg.nickname.unwrap_or_else(Nickname::random);
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let mut elc = setup_swarm(
            SetupKind::Create { mode, name },
            author,
            /* interactive */ false,
            cfg.max_peers,
            /* state_file */ None,
            lookups.clone(),
            Output::capture(events_tx),
        )
        .await?;
        // When advertising, start the re-broadcast task (tied to this
        // session) on the same lookups as the swarm.
        let advertiser =
            directory.map(|directory_name| spawn_advertiser(&mut elc, directory_name, lookups));
        Ok(Self::spawn_session_from(elc, events_rx, advertiser))
    }

    /// Wire the embed channels into `elc`, spawn the event loop, and
    /// build the session handle. Shared by `join` and `create`.
    fn spawn_session_from(
        mut elc: EventLoopConfig,
        events_rx: mpsc::UnboundedReceiver<OutputEvent>,
        advertiser: Option<JoinHandle<()>>,
    ) -> Self {
        let (msg_tx, _initial_rx) = broadcast::channel::<Message>(EMBED_INBOUND_CAP);
        let (send_tx, send_rx) = mpsc::channel::<SendRequest>(32);
        let (quit_tx, quit_rx) = mpsc::channel::<()>(1);

        // A fully in-process session: typed inbound push, a dedicated
        // outbound-send channel, external quit; no socket, no process
        // exit. The `DriverMode` makes that the only representable
        // shape (no stray None/true combination to get wrong).
        elc.driver = DriverMode::Embed {
            msg_tx: msg_tx.clone(),
            send_rx,
            quit_rx,
        };

        // Resolved/effective values — no stdout scraping needed.
        let swarm_id = elc.swarm.clone();
        let nickname = elc.author.clone();

        let task = tokio::spawn(crate::daemon::run(elc));

        Self {
            swarm_id,
            nickname,
            msg_tx,
            send_tx,
            quit_tx,
            events_rx: Some(events_rx),
            task: Some(task),
            advertiser,
        }
    }

    /// Take the captured structured-event stream
    /// ([`OutputEvent`]: `ready`, `message`, `presence`, `peer_*`,
    /// `info`, …). Single-consumer, so this returns the receiver
    /// **once**; subsequent calls return `None`. Mirrors what the CLI
    /// would print, as typed values instead of stdout lines.
    pub fn events(&mut self) -> Option<mpsc::UnboundedReceiver<OutputEvent>> {
        self.events_rx.take()
    }

    /// The resolved swarm identifier.
    #[must_use]
    pub fn swarm_id(&self) -> &SwarmId {
        &self.swarm_id
    }

    /// Our effective nickname in this swarm.
    #[must_use]
    pub fn nickname(&self) -> &Nickname {
        &self.nickname
    }

    /// Subscribe to inbound messages. Each call returns an independent
    /// receiver that sees traffic sent *after* it subscribed, so
    /// subscribe before you expect messages. Includes every kind that
    /// parses — `msg`, `presence` (joined/left/alive), `peer_info`;
    /// filter as needed. Under sustained lag the bounded ring drops
    /// the oldest messages and the receiver observes
    /// [`broadcast::error::RecvError::Lagged`] — by design, so a slow
    /// consumer never stalls the gossip loop.
    #[must_use]
    pub fn messages(&self) -> broadcast::Receiver<Message> {
        self.msg_tx.subscribe()
    }

    /// Build, sign and gossip-broadcast a message. `body` is UTF-8 text
    /// ([`MessageBody::new`] rejects only disallowed control chars).
    /// `reply` addresses it to a specific peer's nickname. Returns the
    /// new message id, or `None` when the sender-side rate limiter
    /// dropped the message (same per-author quota the receiver enforces).
    ///
    /// # Errors
    /// Fails if the event loop has stopped, or if serialization /
    /// gossip broadcast fails inside the loop.
    pub async fn send(
        &self,
        body: MessageBody,
        reply: Option<Nickname>,
    ) -> anyhow::Result<Option<MessageId>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.send_tx
            .send(SendRequest {
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

    /// Clean shutdown: ask the loop to broadcast `Left`, remove its
    /// state file, and return. Waits up to 3s for the task to wind
    /// down; on timeout it returns `Ok(())` and the task is left to
    /// finish detaching.
    ///
    /// # Errors
    /// Returns an error if the event-loop task panicked or returned an
    /// error before shutting down.
    pub async fn leave(mut self) -> anyhow::Result<()> {
        // Stop advertising first — we're leaving the swarm, so its
        // listing should age out rather than keep being re-broadcast.
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

impl Drop for SwarmSession {
    fn drop(&mut self) {
        // Fallback if `leave()` was never called: abort the loop task
        // so it doesn't leak. (Mirrors the MCP session's Drop.)
        if let Some(task) = self.task.take() {
            task.abort();
        }
        if let Some(advertiser) = self.advertiser.take() {
            advertiser.abort();
        }
    }
}

/// Resolve the create-time advertise request into the directory to list in,
/// or `None` when not advertising. Errors if `advertise` is set on a
/// non-public swarm (directory listing requires the public network) or
/// the directory name is invalid. The string-typed surface keeps `embed`
/// iroh-free / clap-free, mirroring `CreateConfig`.
fn resolve_advertise_directory(
    advertise: bool,
    directory: Option<String>,
    mode: SwarmMode,
) -> anyhow::Result<Option<SwarmName>> {
    if !advertise {
        return Ok(None);
    }
    if mode != SwarmMode::Public && !crate::util::tuning::directory_private_for_test() {
        anyhow::bail!("advertise requires the public network");
    }
    let directory_name = match directory {
        Some(value) => SwarmName::new(value)
            .map_err(|error| anyhow::anyhow!("invalid directory name: {error}"))?,
        None => SwarmName::new(DEFAULT_DIRECTORY).expect("DEFAULT_DIRECTORY is a valid swarm name"),
    };
    Ok(Some(directory_name))
}

/// Spawn the directory re-broadcast task for `cfg`'s swarm: wire a fresh
/// live-participant counter into `cfg.live_count`, then re-send the
/// swarm's `ahs…` id (with that count) into `directory` every
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
        let swarm = directory_swarm(&directory);
        // The advertiser is the directory's de-facto origin: co-host its
        // rendezvous *eagerly* (from t=0) so a beacon exists before any
        // discoverer subscribes — the create+join shape that meshes in
        // seconds. (A `Deferred` advertiser only beacons at the first
        // heal tick, after discoverers have already failed their first
        // graft against a dead rendezvous.) In private, claim-if-free
        // elects one beacon among multiple advertisers; in public the
        // common single-advertiser case is what this serves.
        let session = match SwarmSession::join_with_lookups(
            swarm,
            lookups,
            None,
            CoHostPolicy::Eager,
        )
        .await
        {
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
                id: swarm_id.to_string(),
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
/// projection of a [`crate::directory::Listing`].
#[derive(Debug, Clone)]
pub struct SwarmListing {
    /// The advertised swarm's id — pass to [`SwarmSession::join`] to join.
    pub swarm: SwarmId,
    /// Human-readable swarm name (decoded from the id).
    pub name: String,
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
        name: listing.name.as_str().to_owned(),
        public: listing.public,
        peers: listing.peers,
        first_seen_unix: listing.first_seen_unix,
    }
}

/// A live view of a directory. Joins the directory's swarm as an
/// ordinary [`SwarmSession`] and collects advertisements into
/// [`Listings`], aging out swarms whose publishers went silent. Drop
/// (or let it fall out of scope) to leave the directory.
///
/// ```no_run
/// # use agent_habilis_swarm::embed::Directory;
/// # async fn run() -> anyhow::Result<()> {
/// let mut directory = Directory::open(Some("demo")).await?;
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
    /// Open a directory by name. `name` is the directory name; `None` ⇒
    /// the well-known `global` directory. Uses the default all-on public
    /// lookups; for granular control use [`Directory::open_with_lookups`].
    /// Returns once the directory session is ready; listings then
    /// accumulate in the background.
    ///
    /// # Errors
    /// Fails if the name is invalid or the directory session cannot be
    /// established (endpoint/gossip setup, bootstrap unreachable).
    pub async fn open(name: Option<impl Into<String>>) -> anyhow::Result<Self> {
        // Match the directory's mode (public normally; private under the
        // test hook) so the lookups are valid for it.
        let lookups = LookupOpts::default_for(directory::directory_mode(), None);
        Self::open_with_lookups(name, lookups).await
    }

    /// Like [`Directory::open`], but with an explicit lookup set — the
    /// directory's swarm is reached over exactly these lookups (the CLI
    /// passes its `--mdns/--dht/--relay` allowlist here). `pub(crate)`:
    /// keeps `LookupOpts` out of the iroh-free public surface.
    ///
    /// # Errors
    /// Fails if the name is invalid or the directory session cannot be
    /// established (endpoint/gossip setup, bootstrap unreachable).
    ///
    /// # Panics
    /// Panics only if the internal collector mutex is poisoned by a
    /// panic in the background task — not reachable in normal use.
    pub(crate) async fn open_with_lookups(
        name: Option<impl Into<String>>,
        lookups: LookupOpts,
    ) -> anyhow::Result<Self> {
        let directory_name = match name {
            Some(value) => SwarmName::new(value.into())
                .map_err(|error| anyhow::anyhow!("invalid directory name: {error}"))?,
            None => {
                SwarmName::new(DEFAULT_DIRECTORY).expect("DEFAULT_DIRECTORY is a valid swarm name")
            }
        };
        let swarm = directory_swarm(&directory_name);
        // A discoverer is a pure consumer: never co-host the directory's
        // rendezvous (it only dials an advertiser's beacon).
        let session =
            SwarmSession::join_with_lookups(swarm, lookups, None, CoHostPolicy::Never).await?;
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
                                match dir.observe(message.body.as_str(), now) {
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
