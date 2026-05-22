//! In-process embedding facade.
//!
//! [`SwarmSession`] runs the swarm event loop as a background `tokio`
//! task **inside the caller's process** — no subprocess, no Unix-socket
//! IPC. Inbound traffic is pushed over a bounded broadcast channel;
//! outbound sends go through a dedicated channel into the same shared
//! broadcast path the CLI/IPC uses. No `iroh` type is exposed: targets
//! are resolved internally from a string (`ahs…` / domain / git URL).

use std::time::Duration;

use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::daemon::setup::{SetupKind, setup_swarm};
use crate::daemon::{DriverMode, EventLoopConfig, SendRequest};
use crate::output::{Output, OutputEvent};
use crate::protocol::swarm::{DiscoveryOpts, SwarmMode, SwarmName, resolve_relay};
use crate::protocol::{Message, MessageBody, MessageId, Nickname, SwarmId};
use crate::resolver;
use crate::util::tuning::{DEFAULT_MAX_DIRECT_PEERS, EMBED_INBOUND_CAP};

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
    /// `true` = public (cross-machine networking, pkarr discovery); `false`
    /// = private (localhost only). Default `false`.
    pub public: bool,
    /// Custom relay URL, honored only with `public`. `None` uses the
    /// default relay. Parsed internally.
    pub relay: Option<String>,
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
        let discovery = DiscoveryOpts::legacy(swarm.mode, None);
        let author = cfg.nickname.unwrap_or_else(Nickname::random);
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let elc = setup_swarm(
            SetupKind::Join { swarm },
            author,
            /* interactive */ false,
            cfg.max_peers,
            /* state_file */ None,
            discovery,
            Output::capture(events_tx),
        )
        .await?;
        Ok(Self::spawn_session_from(elc, events_rx))
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
        let discovery = DiscoveryOpts::legacy(mode, relay);
        let author = cfg.nickname.unwrap_or_else(Nickname::random);
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let elc = setup_swarm(
            SetupKind::Create { mode, name },
            author,
            /* interactive */ false,
            cfg.max_peers,
            /* state_file */ None,
            discovery,
            Output::capture(events_tx),
        )
        .await?;
        Ok(Self::spawn_session_from(elc, events_rx))
    }

    /// Wire the embed channels into `elc`, spawn the event loop, and
    /// build the session handle. Shared by `join` and `create`.
    fn spawn_session_from(
        mut elc: EventLoopConfig,
        events_rx: mpsc::UnboundedReceiver<OutputEvent>,
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
    /// new message id.
    ///
    /// # Errors
    /// Fails if the event loop has stopped, or if serialization /
    /// gossip broadcast fails inside the loop.
    pub async fn send(
        &self,
        body: MessageBody,
        reply: Option<Nickname>,
    ) -> anyhow::Result<MessageId> {
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
    }
}
