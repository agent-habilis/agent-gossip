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

use crate::output;
use crate::protocol::swarm::SwarmName;
use crate::protocol::{Message, MessageBody, MessageId, Nickname, SwarmId};

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
    /// Co-host from t=0 with no probe — the swarm origin (`create`) and
    /// the advertiser's directory session (the directory's content
    /// server / de-facto origin), so a beacon exists before any joiner
    /// or discoverer subscribes.
    Eager,
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
    pub author: Nickname,
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
