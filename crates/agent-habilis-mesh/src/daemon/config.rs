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
    /// A long-running CLI daemon. Owns the unix-socket IPC listener
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
    /// the rest stay peers and mesh through it, so every advertiser's
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
/// Opaque to consumers: built whole by
/// [`setup_mesh`](super::setup::setup_mesh) and handed to
/// [`Node::spawn`](super::Node::spawn) or [`run`](super::run) unmodified. The
/// fields are crate-private both because most are `iroh` types the surface must
/// not expose, and because three of them used to be patched in after
/// construction — a window in which the value was knowingly wrong.
pub struct EventLoopConfig {
    pub(crate) topic: GossipTopic,
    /// The gossip frontend the topic was subscribed on. Held by the
    /// event loop so it can **re-subscribe** after the topic stream
    /// terminally ends — iroh-gossip closes a lagging subscriber and
    /// its docs say to re-open it; without this handle the daemon
    /// would stay permanently deaf (review finding H1).
    pub(crate) gossip: iroh_gossip::net::Gossip,
    pub(crate) author: Nickname,
    /// This member's signing identity (Ed25519), minted in `setup_mesh`.
    /// In-process / ephemeral for now (see [`crate::protocol::identity`]).
    pub(crate) identity: std::sync::Arc<crate::protocol::identity::Identity>,
    pub(crate) mesh: MeshId,
    /// Decoded mesh name (from the `💬…` id). Carried so the
    /// shutdown path can print `left #NAME` without re-parsing
    /// the id.
    pub(crate) name: MeshName,
    /// The raw string a topic mesh was derived from; `None` for create/join.
    /// The derived `name` is lossy (URL scheme and query stripped, truncated),
    /// so this is what the joined/left lines, the state file, and `leave`'s
    /// report show for a topic gossip.
    pub(crate) topic_string: Option<String>,
    /// The raw mesh password, retained for the process lifetime when the
    /// mesh is password-protected (`None` otherwise). Needed at blob-offload
    /// time to key blob tickets with the same password — the Argon2id stretch
    /// takes the raw password string, which cannot be recovered from
    /// `mesh_key`. `Password`'s `Debug`/`Display` redact to `***`.
    pub(crate) mesh_password: Option<crate::protocol::crypto::Password>,
    /// The Argon2id-stretched mesh key (`Mesh::stretched_key`), retained to
    /// derive the per-channel keys that encrypt the `state`/`meta` docs and
    /// broadcast chat. `None` for a passwordless mesh — those stay plaintext.
    /// Wiped on drop.
    pub(crate) mesh_key: Option<zeroize::Zeroizing<[u8; 32]>>,
    /// Per-loop generic event sink (the tapped `Output`, wrapped as a
    /// [`NodeSink`]). The engine emits `NodeEvent`s through it and hands a clone
    /// to every handler, so multiple in-process sessions each have their own and
    /// never race a shared global.
    pub(crate) sink: std::sync::Arc<dyn NodeSink>,
    /// The invite-only **creator's** mesh, retained (in-memory, secrets and
    /// all) so the `invite` command can mint from its issuer key + root. `Some`
    /// only on the creator of an invite-only mesh; `None` everywhere else (a
    /// joiner holds no issuer key, so it could never mint).
    pub(crate) mint_mesh: Option<Mesh>,
    pub(crate) endpoint: Endpoint,
    /// iroh router whose accept loop routes inbound gossip
    /// connections. Must be held alive for the whole event loop —
    /// dropping it kills the accept task and makes the daemon
    /// unreachable to new peers.
    pub(crate) router: Router,
    pub(crate) max_peers: usize,
    /// Inputs for (re)building the co-hosted rendezvous endpoint.
    /// `rendezvous_params.id` doubles as the bootstrap-cache heal
    /// anchor and the peer-side neighbor-filter id;
    /// `beacon::ensure` is called with these on startup and every
    /// heal tick (claim-if-free in private mode).
    pub(crate) rendezvous_params: beacon::RendezvousParams,
    /// Receives the bootstrap relay rung chosen off the event loop — by
    /// the backgrounded startup probe and the beacon's liveness
    /// self-monitor (`rendezvous_params.rung_tx` is the sending half). On
    /// a change the loop re-registers the rendezvous and re-homes the
    /// beacon, so the ladder walk never runs on the sole loop.
    pub(crate) rung_rx: watch::Receiver<Option<iroh::RelayUrl>>,
    /// When this member may serve the rendezvous (beacon role).
    pub(crate) cohost: CoHostPolicy,
    /// The consumer's per-user runtime base — the root the control socket, the
    /// default state file, and the default log dir live under. Carried from
    /// [`SetupParams`](super::setup::SetupParams) rather than derived here; see
    /// [`runtime_base`](crate::util::runtime_base) for why it is supplied.
    /// `None` for an embedder that writes no files and binds no socket.
    pub(crate) runtime_base: Option<PathBuf>,
    /// When set, the daemon writes peer count changes to this file.
    pub(crate) state_file: Option<PathBuf>,
    /// The multi-hop transport handle when `--multihop` registered it on the
    /// peer endpoint; `run()` moves it into `EventLoopState::multihop`.
    /// `None` when multihop is off. Built in `setup_mesh`.
    pub(crate) multihop: Option<iroh_multihop_transport::MultihopHandle>,
    /// Inbound unicast frames from the `UNICAST_ALPN` acceptor. The event loop
    /// drains this into `gossip::ingest` (the same path as gossip), so both
    /// transports share signature-verify + dedup. Built in `setup_mesh`.
    pub(crate) unicast_rx: mpsc::Receiver<bytes::Bytes>,
    /// When advertising (`create --advertise`), the shared counter the
    /// directory re-broadcast task reads the live peer count
    /// from. `setup_mesh` leaves this `None`; the advertise path sets
    /// it before `run` (same late-assignment pattern as `driver`).
    pub(crate) live_count: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    /// Who drives the loop (CLI / in-process) and the channels that driver
    /// needs. Assigned by [`Node::spawn`](super::Node::spawn) for an in-process
    /// consumer; a config handed straight to [`run`](super::run) keeps the CLI
    /// default. Crate-private, so it cannot be the wrong variant in a window
    /// visible to a consumer.
    pub(crate) driver: DriverMode,
    /// The `meta` channel's per-peer write gate, when the application keeps a
    /// per-peer map there (see [`crate::doc::SelfWriteGate`]). `None` leaves the
    /// channel free-form.
    pub(crate) per_peer_gate: Option<crate::doc::SelfWriteGate>,
}

impl EventLoopConfig {
    /// The resolved mesh id. Available before the loop starts, for a consumer
    /// that must name the mesh at setup time (a log attach, a directory ad).
    #[must_use]
    pub fn mesh_id(&self) -> &MeshId {
        &self.mesh
    }

    /// This member's nickname, as resolved by setup.
    #[must_use]
    pub fn author(&self) -> &Nickname {
        &self.author
    }
}

impl std::fmt::Debug for EventLoopConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventLoopConfig").finish_non_exhaustive()
    }
}
