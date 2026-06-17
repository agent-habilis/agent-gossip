//! Inputs the event loop is constructed from.
//!
//! [`EventLoopConfig`] is produced by `daemon::setup::setup_swarm` and
//! consumed by [`daemon::run`](super::run); [`SessionRequest`] is the
//! typed in-process send/poll message (embed + MCP). Split out of
//! `mod.rs` so the orchestrator file reads as pure lifecycle narrative.

use std::path::PathBuf;

use anyhow::Result;
use iroh::{Endpoint, protocol::Router};
use iroh_gossip::api::GossipTopic;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::daemon::state::RosterSnapshot;
use crate::output;
use crate::protocol::swarm::SwarmName;
use crate::protocol::{
    ExchangeId, ExchangeKind, ExchangePhase, Message, MessageBody, MessageId, Nickname, SwarmId,
};

use crate::beacon;

/// A typed in-process request from an embed/MCP session to the event
/// loop — the shared alternative to the CLI's `IpcCommand`-over-socket
/// (which must serialize). `Send` broadcasts a message and echoes back the
/// canonical [`Message`] (`None` ⇒ dropped by the sender-side rate
/// limiter); `Poll` reads the buffered history after a cursor.
pub(crate) enum SessionRequest {
    Send {
        body: MessageBody,
        reply: Option<Nickname>,
        resp: oneshot::Sender<Result<Option<Message>>>,
    },
    Poll {
        after: Option<MessageId>,
        resp: oneshot::Sender<Vec<Message>>,
    },
    /// Send one leg of an exchange to `to`, correlated by `exchange_id`.
    /// Echoes back the canonical [`Message`] (`None` ⇒ dropped by the
    /// sender-side rate limiter), like [`Send`](SessionRequest::Send).
    /// Addressee validation for `Offer` lives in `broadcast_exchange`.
    Exchange {
        to: Nickname,
        exchange_id: ExchangeId,
        kind: ExchangeKind,
        phase: ExchangePhase,
        body: MessageBody,
        resp: oneshot::Sender<Result<Option<Message>>>,
    },
    /// Snapshot the live participant roster (active + quiet, recency-sorted).
    Peers {
        resp: oneshot::Sender<RosterSnapshot>,
    },
    /// Broadcast pre-built wire bytes **verbatim** — no signing, no chain
    /// stamping. The escape hatch the `adversarial` feature uses to inject
    /// crafted/malicious messages (bad signature, equivocation, backdating)
    /// that a correct client would never produce, so the adversarial test
    /// suite can prove receivers reject/flag them. Reachable only through
    /// the adversarial-gated `SwarmSession::inject_raw`.
    #[cfg(feature = "adversarial")]
    InjectRaw { bytes: bytes::Bytes },
    /// Snapshot the fork/DAG index sizes `(by_hash, dag_heads, author_seqs)`.
    /// Adversarial-suite only — lets it assert that messages we don't
    /// retain (rate-dropped, or replies addressed to another peer) are never
    /// folded into the indexes, i.e. the unbounded-growth leak is closed.
    #[cfg(feature = "adversarial")]
    IndexStats {
        resp: oneshot::Sender<(usize, usize, usize)>,
    },
    /// Simulate the gossip event stream terminally ending (flips
    /// `gossip_open` off, exactly what the real `None` arm does — the
    /// orphaned receiver is dropped when the heal arm resubscribes).
    /// Adversarial-suite only: drives the stream-end recovery test
    /// without needing to kill the gossip actor from outside.
    #[cfg(feature = "adversarial")]
    SeverGossip,
}

/// Who drives the event loop. The variants make illegal channel
/// combinations unrepresentable and let the loop *derive* both "exit the
/// process on quit?" and "spawn the unix-socket listener?" instead of
/// carrying them as independent, drift-prone bools.
pub(crate) enum DriverMode {
    /// The `ahs create` / `join` CLI. Owns the unix-socket IPC listener
    /// (for `msg` / `poll`); ctrl-c / SIGTERM `std::process::exit`s.
    Cli,
    /// Fully in-process, shared by the embed facade and the MCP server.
    /// Outbound sends + polls arrive **typed** on `req_rx`; `msg_tx`
    /// pushes inbound to a broadcast subscriber (embed's `messages()`),
    /// or is `None` for a poll-only consumer (the MCP server). Never
    /// exits the process and binds no socket; shutdown on `quit_rx`.
    InProcess {
        msg_tx: Option<broadcast::Sender<Message>>,
        req_rx: mpsc::Receiver<SessionRequest>,
        quit_rx: mpsc::Receiver<()>,
    },
}

/// When a member may co-host (serve) the seed-derived rendezvous — the
/// **beacon** role. Co-hosting a duplicate `rendezvous_id` before the
/// member has meshed registers a second copy on the shared pinned relay
/// and can capture the member's own bootstrap dial → isolation; the
/// policy plus the public probe-before-claim (`beacon::ensure`) keep
/// that from happening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoHostPolicy {
    /// Co-host from t=0 with no probe — the swarm origin (`create`),
    /// which has no peers to collide with, so a beacon exists before any
    /// joiner subscribes.
    Eager,
    /// Co-host from t=0 like [`Eager`](CoHostPolicy::Eager), but
    /// **probe-before-claim** so concurrent claimants on a *shared*
    /// rendezvous don't collide — the directory advertiser, where several
    /// swarms advertising into the same directory share one seed-derived
    /// `rendezvous_id`. The first to start claims (its probe finds nothing);
    /// the rest stay participants and mesh through it, so every advertiser's
    /// ad reaches discoverers.
    EagerProbed,
    /// Co-host once meshed, or speculatively after the empty-swarm
    /// grace (probe-gated in public) — a normal joiner.
    Deferred,
    /// Never co-host — a pure consumer (the `discover` directory
    /// session). It only ever dials an existing beacon.
    Never,
}

/// Configuration for the event loop, shared by `create` and `join`.
/// The driver-specific channels live in [`DriverMode`].
pub(crate) struct EventLoopConfig {
    pub topic: GossipTopic,
    /// The gossip frontend the topic was subscribed on. Held by the
    /// event loop so it can **re-subscribe** after the topic stream
    /// terminally ends — iroh-gossip closes a lagging subscriber and
    /// its docs say to re-open it; without this handle the daemon
    /// would stay permanently deaf (review finding H1).
    pub gossip: iroh_gossip::net::Gossip,
    pub author: Nickname,
    /// This member's signing identity (Ed25519), minted in `setup_swarm`.
    /// In-process / ephemeral for now (see [`crate::protocol::identity`]).
    pub identity: std::sync::Arc<crate::protocol::identity::Identity>,
    pub swarm: SwarmId,
    /// Decoded swarm name (from the `ahs…` id). Carried so the
    /// shutdown path can print `left #NAME` without re-parsing
    /// the id.
    pub name: SwarmName,
    /// Per-loop output sink. Threaded to every handler so multiple
    /// in-process sessions each have their own and never race a shared
    /// global.
    pub output: output::Output,
    pub interactive: bool,
    /// This node's self-reported model / harness (`--model`/`--harness`),
    /// announced in our `joined` body so peers can show what we run on.
    /// `None` when the flag was omitted.
    pub model: Option<String>,
    pub harness: Option<String>,
    pub endpoint: Endpoint,
    /// iroh router whose accept loop routes inbound gossip
    /// connections. Must be held alive for the whole event loop —
    /// dropping it kills the accept task and makes the daemon
    /// unreachable to new peers.
    pub router: Router,
    pub max_peers: usize,
    /// Per-author messages-per-minute cap decoded from the swarm id
    /// (`0` ⇒ no rate limit). Uniform across the swarm because it travels
    /// in the hash; the event loop builds the `SwarmRateLimiter` from it.
    pub rate_limit_per_min: u16,
    /// Inputs for (re)building the co-hosted rendezvous endpoint.
    /// `rendezvous_params.id` doubles as the bootstrap-cache heal
    /// anchor and the participant-side neighbor-filter id;
    /// `beacon::ensure` is called with these on startup and every
    /// heal tick (claim-if-free in private mode).
    pub rendezvous_params: beacon::RendezvousParams,
    /// Receives the bootstrap relay rung chosen off the event loop — by
    /// the backgrounded startup probe and the beacon's liveness
    /// self-monitor (`rendezvous_params.rung_tx` is the sending half). On
    /// a change the loop re-registers the rendezvous and re-homes the
    /// beacon, so the ladder walk never runs on the sole loop.
    pub rung_rx: watch::Receiver<Option<iroh::RelayUrl>>,
    /// When this member may serve the rendezvous (beacon role).
    pub cohost: CoHostPolicy,
    /// When set, the daemon writes peer count changes to this file.
    pub state_file: Option<PathBuf>,
    /// When advertising (`create --advertise`), the shared counter the
    /// directory re-broadcast task reads the live participant count
    /// from. `setup_swarm` leaves this `None`; the advertise path sets
    /// it before `run` (same late-assignment pattern as `driver`).
    pub live_count: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    /// Who drives the loop (CLI / MCP / embed) and the channels that
    /// driver needs. `setup_swarm` leaves this [`DriverMode::Cli`] and
    /// the MCP / embed sessions reassign it before `run`; folding the
    /// driver into the `setup_swarm` signature (so it can't be the
    /// wrong variant for a window) is the remaining follow-up.
    pub driver: DriverMode,
}
