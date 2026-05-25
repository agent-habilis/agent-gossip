//! Inputs the event loop is constructed from.
//!
//! [`EventLoopConfig`] is produced by `daemon::setup::setup_swarm` and
//! consumed by [`daemon::run`](super::run); [`SendRequest`] is the
//! embed facade's outbound-send message. Split out of `mod.rs` so the
//! orchestrator file reads as pure lifecycle narrative.

use std::path::PathBuf;

use anyhow::Result;
use iroh::{Endpoint, protocol::Router};
use iroh_gossip::api::GossipTopic;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::output;
use crate::protocol::swarm::SwarmName;
use crate::protocol::{Message, MessageBody, MessageId, Nickname, SwarmId};
use crate::transport::ipc;

use crate::beacon;

/// An outbound send request from the embed facade. `resp` carries the
/// new message id back to the caller — `None` when the send was dropped
/// by the sender-side rate limiter — or the build/broadcast error.
pub(crate) struct SendRequest {
    pub body: MessageBody,
    pub reply: Option<Nickname>,
    pub resp: oneshot::Sender<Result<Option<MessageId>>>,
}

/// Who drives the event loop. The three variants make illegal channel
/// combinations unrepresentable (e.g. an embed session can't exist
/// without its send/quit channels) and let the loop *derive* both
/// "exit the process on quit?" and "spawn the unix-socket listener?"
/// instead of carrying them as independent, drift-prone bools.
pub(crate) enum DriverMode {
    /// The `ahs create` / `join` CLI. Owns the
    /// unix-socket IPC listener (for `msg` / `poll`); ctrl-c / SIGTERM
    /// `std::process::exit`s.
    Cli,
    /// The MCP stdio server: drives the loop in-process with a
    /// pre-wired IPC command channel and an external quit. Never exits
    /// the process (one swarm of potentially many in the server) and
    /// binds no socket (commands arrive on `ipc_rx`).
    Mcp {
        ipc_rx: mpsc::Receiver<ipc::IpcMessage>,
        quit_rx: mpsc::Receiver<()>,
    },
    /// The embed facade: fully in-process. Inbound traffic is pushed
    /// on `msg_tx`, outbound sends arrive on `send_rx`, shutdown on
    /// `quit_rx`. No socket, no process exit.
    Embed {
        msg_tx: broadcast::Sender<Message>,
        send_rx: mpsc::Receiver<SendRequest>,
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
    /// Per-loop output sink (replaces the former process-global
    /// output mode/filter statics). Threaded to every handler so
    /// multiple in-process sessions don't race a shared `OnceLock`.
    pub output: output::Output,
    pub interactive: bool,
    pub endpoint: Endpoint,
    /// iroh router whose accept loop routes inbound gossip
    /// connections. Must be held alive for the whole event loop —
    /// dropping it kills the accept task and makes the daemon
    /// unreachable to new peers.
    pub router: Router,
    pub max_peers: usize,
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
