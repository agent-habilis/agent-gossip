//! Inputs the event loop is constructed from.
//!
//! [`EventLoopConfig`] is produced by `daemon::setup::setup_mesh` and
//! consumed by [`daemon::run`](super::run). Split out of `mod.rs` so the
//! orchestrator file reads as pure lifecycle narrative.

use std::path::PathBuf;

use iroh::{Endpoint, protocol::Router};
use iroh_gossip::api::GossipTopic;
use tokio::sync::{broadcast, mpsc, watch};

use crate::gossip::event::NodeSink;
use crate::protocol::mesh::{Mesh, MeshName};
use crate::protocol::{MeshId, Message, Nickname};

use crate::beacon;

/// Who drives the event loop. The variants make illegal channel
/// combinations unrepresentable and let the loop *derive* both "exit the
/// process on quit?" and "spawn the unix-socket listener?" instead of
/// carrying them as independent, drift-prone bools.
#[derive(Debug)]
pub enum DriverMode {
    /// The `agent-square create` / `join` CLI. Owns the unix-socket IPC listener
    /// (for `msg` / `poll`); ctrl-c / SIGTERM `std::process::exit`s.
    Cli,
    /// Fully in-process, driven by a [`Node`](super::Node). Outbound sends +
    /// polls arrive **typed** on the `session_rx` that [`run`](super::run) takes
    /// alongside this; `msg_tx` fans inbound out to the consumer's broadcast, or
    /// is `None` when the consumer drains frames some other way (a poll-only
    /// session, or an app that consumes every frame inside the loop). Never
    /// exits the process and binds no socket; shutdown on `quit_rx`.
    InProcess {
        msg_tx: Option<broadcast::Sender<Message>>,
        quit_rx: mpsc::Receiver<()>,
        /// Whether this session registers the process-wide
        /// ctrl-c/SIGTERM/SIGHUP/SIGQUIT listeners for a graceful leave.
        /// Registering any tokio signal handler suppresses the OS
        /// default-terminate for the *whole process, permanently* — so a
        /// session living inside a foreground command that owns its own
        /// lifetime (a `--advertise` transfer's directory session, a
        /// directory browse) must pass `false`, or ctrl-c stops killing
        /// the host command.
        handle_signals: bool,
    },
}

/// When a member may co-host (serve) the seed-derived rendezvous — the
/// **beacon** role. Co-hosting a duplicate `rendezvous_id` before the
/// member has meshed registers a second copy on the shared pinned relay
/// and can capture the member's own bootstrap dial → isolation; the
/// policy plus the public probe-before-claim (`beacon::ensure`) keep
/// that from happening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoHostPolicy {
    /// Co-host from t=0 with no probe — the mesh origin (`create`),
    /// which has no peers to collide with, so a beacon exists before any
    /// joiner subscribes.
    Eager,
    /// Co-host from t=0 like [`Eager`](CoHostPolicy::Eager), but
    /// **probe-before-claim** so concurrent claimants on a *shared*
    /// rendezvous don't collide — the directory advertiser, where several
    /// meshes advertising into the same directory share one seed-derived
    /// `rendezvous_id`. The first to start claims (its probe finds nothing);
    /// the rest stay participants and mesh through it, so every advertiser's
    /// ad reaches discoverers.
    EagerProbed,
    /// Co-host once meshed, or speculatively after the empty-mesh
    /// grace (probe-gated in public) — a normal joiner.
    Deferred,
    /// Never co-host — a pure consumer (the `discover` directory
    /// session). It only ever dials an existing beacon.
    Never,
}

/// The co-host policy a directory advertiser runs: co-host the shared
/// rendezvous from t=0 but **probe-before-claim**, so a second advertiser into
/// the same directory defers instead of binding a duplicate (which partitioned
/// the directory in public mode — only one mesh was discoverable).
pub const DIRECTORY_ADVERTISER_COHOST: CoHostPolicy = CoHostPolicy::EagerProbed;

/// Configuration for the event loop, shared by `create` and `join`.
/// The driver-specific channels live in [`DriverMode`].
pub struct EventLoopConfig {
    pub topic: GossipTopic,
    /// The gossip frontend the topic was subscribed on. Held by the
    /// event loop so it can **re-subscribe** after the topic stream
    /// terminally ends — iroh-gossip closes a lagging subscriber and
    /// its docs say to re-open it; without this handle the daemon
    /// would stay permanently deaf (review finding H1).
    pub gossip: iroh_gossip::net::Gossip,
    pub author: Nickname,
    /// This member's signing identity (Ed25519), minted in `setup_mesh`.
    /// In-process / ephemeral for now (see [`crate::protocol::identity`]).
    pub identity: std::sync::Arc<crate::protocol::identity::Identity>,
    pub mesh: MeshId,
    /// Decoded mesh name (from the `💬…` id). Carried so the
    /// shutdown path can print `left #NAME` without re-parsing
    /// the id.
    pub name: MeshName,
    /// The raw mesh password, retained for the process lifetime when the
    /// mesh is password-protected (`None` otherwise). Needed at blob-offload
    /// time to key blob tickets with the same password — the Argon2id stretch
    /// takes the raw password string, which cannot be recovered from
    /// `mesh_key`. `Password`'s `Debug`/`Display` redact to `***`.
    pub mesh_password: Option<crate::protocol::crypto::Password>,
    /// The Argon2id-stretched mesh key (`Mesh::stretched_key`), retained to
    /// derive the per-channel keys that encrypt the `state`/`meta` docs and
    /// broadcast chat. `None` for a passwordless mesh — those stay plaintext.
    /// Wiped on drop.
    pub mesh_key: Option<zeroize::Zeroizing<[u8; 32]>>,
    /// Per-loop generic event sink (the tapped `Output`, wrapped as a
    /// [`NodeSink`]). The engine emits `NodeEvent`s through it and hands a clone
    /// to every handler, so multiple in-process sessions each have their own and
    /// never race a shared global.
    pub sink: std::sync::Arc<dyn NodeSink>,
    /// The invite-only **creator's** mesh, retained (in-memory, secrets and
    /// all) so the `invite` command can mint from its issuer key + root. `Some`
    /// only on the creator of an invite-only mesh; `None` everywhere else (a
    /// joiner holds no issuer key, so it could never mint).
    pub mint_mesh: Option<Mesh>,
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
    /// Which transports directed messages may use (per-session). `run()` copies
    /// it into `EventLoopState::transport`, which `unicast::deliver` reads.
    pub transport: crate::transport::TransportPolicy,
    /// The multi-hop transport handle when `--multihop` registered it on the
    /// participant endpoint; `run()` moves it into `EventLoopState::multihop`.
    /// `None` when multihop is off. Built in `setup_mesh`.
    pub multihop: Option<iroh_multihop_transport::MultihopHandle>,
    /// Inbound unicast frames from the `UNICAST_ALPN` acceptor. The event loop
    /// drains this into `gossip::ingest` (the same path as gossip), so both
    /// transports share signature-verify + dedup. Built in `setup_mesh`.
    pub unicast_rx: mpsc::Receiver<bytes::Bytes>,
    /// When advertising (`create --advertise`), the shared counter the
    /// directory re-broadcast task reads the live participant count
    /// from. `setup_mesh` leaves this `None`; the advertise path sets
    /// it before `run` (same late-assignment pattern as `driver`).
    pub live_count: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    /// Who drives the loop (CLI / in-process) and the channels that
    /// driver needs. `setup_mesh` leaves this [`DriverMode::Cli`] and
    /// the in-process sessions reassign it before `run`; folding the
    /// driver into the `setup_mesh` signature (so it can't be the
    /// wrong variant for a window) is the remaining follow-up.
    pub driver: DriverMode,
    /// Carried from `setup_mesh` to `run` purely so the `ready` event can be
    /// emitted at the point the daemon can actually serve, rather than in
    /// setup — where it announced a socket that was not yet bound.
    pub ready: ReadyAnnounce,
}

/// The `ready` event's payload that only setup knows. Everything else it
/// needs (mesh id, name, nickname) `run` already holds.
#[derive(Debug, Default)]
pub struct ReadyAnnounce {
    /// A stale skill install, rendered by `agent-square`'s `drift_warning`.
    pub drift: Option<String>,
    /// The bound A2A HTTP port under `--a2a-serve`.
    pub a2a_port: Option<u16>,
}

impl std::fmt::Debug for EventLoopConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventLoopConfig").finish_non_exhaustive()
    }
}
