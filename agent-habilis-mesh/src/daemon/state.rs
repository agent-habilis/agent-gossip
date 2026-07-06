use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::time::Instant as TokioInstant;

use bytes::Bytes;
use iroh::EndpointId;
use serde::Serialize;

use super::bounded_id_set::BoundedIdSet;
use super::message_log::MessageLog;
use crate::daemon::state_file::StateFile;
use crate::protocol::identity::Identity;
use crate::protocol::mesh::Mesh;
use crate::protocol::{Message, Nickname, ShardGroup};
use crate::util::bounded_fifo_set::BoundedFifoSet;
use crate::util::bounded_queue::BoundedQueue;
use crate::util::cooldown::Cooldown;

use crate::util::tuning::{
    KNOWN_ENDPOINTS_CAP, PENDING_OUTBOUND_CAP, QUIET_CAP, RELINK_COOLDOWN_SECS, message_log_size,
    seen_ids_cap,
};

/// `RELINK_COOLDOWN_SECS` as a `Duration` — the window both per-endpoint
/// throttles (`relink`, `peerinfo`) use.
const RELINK_COOLDOWN: Duration = Duration::from_secs(RELINK_COOLDOWN_SECS);

/// How we currently reach a participant: `Direct` when we hold a live gossip
/// link to its self-advertised endpoint, else `Gossip` (relayed). Derived
/// from `linked_endpoints` without surfacing the node id — see
/// `participant_endpoints`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Reach {
    Direct,
    Gossip,
}

/// One participant in a [`RosterSnapshot`]. `last_seen_secs_ago` is
/// `None` until the peer's first heartbeat is timed; `quiet` marks a
/// peer heartbeat-evicted past `ALIVE_TIMEOUT_SECS` (still returnable);
/// `reach` is `direct` only while we hold a live link to it; `transport`
/// is the lane a directed frame to this peer would take right now (the
/// live [`crate::unicast`] send decision — distinct from `reach`, which
/// is a gossip-overlay link fact). Serialized directly into the
/// `agent-square peers` response and the MCP `mesh_info` roster.
#[derive(Debug, Clone, Serialize)]
pub struct RosterEntry {
    pub nickname: Nickname,
    pub last_seen_secs_ago: Option<u64>,
    pub quiet: bool,
    pub reach: Reach,
    pub transport: crate::unicast::Lane,
}

/// Live participant roster: every known peer (active + quiet) sorted
/// most-recently-seen first, plus `count == participants.len() + 1` (the
/// `+1` is self — same definition as the statusline `participant_count`).
#[derive(Debug, Clone, Serialize)]
pub struct RosterSnapshot {
    pub participants: Vec<RosterEntry>,
    pub count: usize,
}

/// All mutable state owned by the event loop.
///
/// Grouped into a single struct so handlers and timer ticks can take
/// `&mut EventLoopState` instead of each borrowing half a dozen
/// independent locals.
///
/// The peer-tracking fields are deliberately one-per-layer (see the
/// Concept Glossary in AGENTS.md): `linked_endpoints` (transport),
/// `participants` (membership roster), `surfaced` (presentation gate),
/// `quiet` (heartbeat-evicted). Never conflate them — they are keyed
/// differently (node id vs nickname) and have different lifetimes.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent lifecycle edges (gossip_open/rendezvous_linked/announced/meshed/degraded) plus the one-shot resident_memory_warned latch; each tracks a distinct transition, not a config bundle worth a sub-struct"
)]
#[derive(Debug)]
pub struct EventLoopState {
    /// Transport layer: the **live** gossip neighbor links, written only
    /// by `NeighborUp`/`NeighborDown` (a received `PeerInfo` is a dial
    /// hint, not a link). Link *truth* matters: the silent-partition WARN
    /// and the healer's re-bridge gate read this set, and an optimistic
    /// entry for a link that never formed has no `NeighborDown` to remove
    /// it — a permanent ghost that suppressed both (the 2026-06-12
    /// roster-collapse). Bounded by HyParView's `active_view_capacity`.
    /// Distinct from `participants` — links are asymmetric and node-id
    /// keyed; the roster is symmetric and nickname keyed.
    pub linked_endpoints: HashSet<EndpointId>,
    /// Re-bridge memory: every peer `EndpointId` we've ever linked to,
    /// kept *across* `NeighborDown` (unlike `linked_endpoints`). When a
    /// node loses all links because the rendezvous/relay is unreachable,
    /// the healer re-dials these directly — iroh still holds their cached
    /// addresses — so the re-bridge no longer depends on the rendezvous.
    /// Bounded FIFO (cap `KNOWN_ENDPOINTS_CAP`) so it can't grow without limit.
    pub known_endpoints: BoundedFifoSet<EndpointId>,
    /// Per-endpoint re-link throttle: caps re-dialing + re-flooding a peer
    /// learned via `PeerInfo` to once per window. Tracked *across*
    /// `NeighborDown` (unlike `linked_endpoints`), so a flapping/unstable peer
    /// is re-linked at most once per `RELINK_COOLDOWN_SECS` — the choke that
    /// stops one bad node's flap from amplifying into a mesh-wide connection
    /// storm. Bounded by construction (see [`Cooldown`]).
    pub relink: Cooldown<EndpointId>,
    /// Per-endpoint `PeerInfo` re-flood throttle. `relink` only throttles the
    /// *inbound* re-dial in `handle_peer_info`; this throttles the *outbound*
    /// re-flood every `NeighborUp` would otherwise trigger (`gossip::recv`).
    /// Without it a single flapping link re-floods the whole mesh on every
    /// up-transition — the residual amplifier behind the soak's ~7.4k-per-host
    /// `neighbor up` storm. Kept separate from `relink` so the two throttles
    /// stay independently reasoned (and a new neighbor still gets exactly one
    /// re-flood).
    pub peerinfo: Cooldown<EndpointId>,
    /// Membership layer: the participant roster. Nickname-keyed set of
    /// other participants, feeding the state file's `participant_count`
    /// (`participants.len() + 1`). Excludes self. Driven by
    /// `joined`/`left` presence messages (they reach every participant
    /// via gossip; `NeighborUp`/`PeerInfo` are asymmetric for
    /// bootstrap links), by implicit heartbeats from any received
    /// message (self-heal for a missed Joined), and by the sweeper's
    /// eviction of participants gone silent past `ALIVE_TIMEOUT_SECS`.
    pub participants: HashSet<Nickname>,
    /// Implicit-heartbeat tracker: nickname -> last time we heard
    /// anything from that participant. Drives sweep-based eviction.
    pub last_seen: HashMap<Nickname, Instant>,
    /// Bridge from membership back to transport: nickname -> the endpoint id
    /// that nickname last self-advertised in a signed `PeerInfo`. That
    /// signature is the only thing tying a nickname to an endpoint (the
    /// signing key is deliberately not the transport key), so this is the one
    /// place the roster can tell whether a participant is a live link.
    /// Last-writer-wins (a restart re-advertises a new endpoint under the same
    /// name). Pruned with `participants`, so it stays bounded by the roster.
    /// Feeds only the derived `reach` boolean in `roster_snapshot` — the node
    /// id never leaves this layer.
    pub participant_endpoints: HashMap<Nickname, EndpointId>,
    /// The mesh's rendezvous endpoint id, once known. Paired with
    /// `rendezvous_linked` so `reach_of` can count the rendezvous link as a
    /// live link to the beacon: the beacon gossips *as* the rendezvous, so a
    /// joiner's only link to it is that pseudo-node link (kept out of
    /// `linked_endpoints` by design). `None` until the loop learns it.
    pub rendezvous_id: Option<EndpointId>,
    /// Heartbeat layer: participants we've evicted as quiet (silent
    /// past `ALIVE_TIMEOUT_SECS`) but who may still reappear. Any
    /// message from a nickname in this set triggers a symmetric
    /// `peer_return` event and re-inclusion in `participants`. Bounded FIFO
    /// (cap `QUIET_CAP`): drained on return, so without the cap a churn /
    /// sybil stream of one-shot nicknames would grow it without bound.
    pub quiet: BoundedFifoSet<Nickname>,
    /// Recency companion to `quiet`: nickname -> the `last_seen` instant a
    /// quiet peer had when it was evicted, so [`roster_snapshot`] can still
    /// report how long ago it was last heard (the eviction drops its
    /// `last_seen` entry). Pruned to `quiet`'s membership on every sweep, so
    /// it stays bounded by `QUIET_CAP`; `roster_snapshot` only reads it for
    /// peers currently in `quiet`, so a stale entry never surfaces.
    pub quiet_since: HashMap<Nickname, Instant>,
    /// Presentation layer: participants for whom we have *surfaced* an
    /// arrival (synthetic `joined`, real `Presence::Joined`, or
    /// `peer_return`). Gates departure surfacing so a participant whose
    /// join we never showed never produces a `went quiet` / `has left`
    /// line — keeps the join-horizon view symmetric. Presentation-only:
    /// the roster (`participants`) stays complete for
    /// anti-entropy/membership; `surfaced ⊆ participants` only governs
    /// what the operator/agent is shown.
    pub surfaced: HashSet<Nickname>,
    /// Last time we broadcast anything. Lets idle daemons suppress
    /// their `Alive` keepalive when they're already chatty.
    pub last_sent_at: Instant,
    /// Wall-clock unix seconds when this daemon started — its join
    /// instant. The node still receives & relays older messages
    /// (anti-entropy keeps the mesh's set uniform), but messages
    /// stamped before this are never *surfaced* (printed / `poll` /
    /// `fetch` / embed): the operator/agent view starts at join.
    pub joined_at: i64,
    /// Goes false when the receiver stream terminally ends; IPC
    /// keeps working for `msg` / `poll` after that.
    pub gossip_open: bool,
    /// Whether the gossip overlay currently links us to the co-hosted
    /// rendezvous (its `NeighborUp`/`NeighborDown` arms drive it). Gates
    /// the healer's connect-probe: probing the rendezvous while a live
    /// link exists makes the beacon's gossip *adopt* the probe
    /// connection and then mark the peer down when the probe handle
    /// drops — one rendezvous-link flap per heal tick, forever (the
    /// 2026-05-30 soak's residual flap).
    pub rendezvous_linked: bool,
    /// Set once we've broadcast our arrival (`joined` + `PeerInfo`).
    /// The announce is deferred to the first `NeighborUp` so it isn't
    /// lost into an unconnected overlay; subsequent neighbors only get
    /// a (log-invisible) `PeerInfo` re-send.
    pub announced: bool,
    /// Set once we have a link to a *real participant* (a non-
    /// rendezvous `NeighborUp`). `announced` flips on any neighbor —
    /// including the rendezvous relay — which is too early to deliver
    /// user content (the relay path may not be converged). Outbound
    /// user messages are buffered until `meshed`, then flushed, so the
    /// first message after a join can't be a lost one-shot.
    pub meshed: bool,
    /// Which transports a directed message may use (per-session). `new()`
    /// defaults to all-enabled; `event_loop::run` sets the configured value.
    /// Read by `crate::unicast::deliver`.
    pub transport: crate::transport::TransportPolicy,
    /// The unicast (point-to-point) connection pool: dials + reuses a QUIC
    /// connection to each addressable participant so a directed message goes
    /// p2p instead of flooding the gossip mesh. Interior-mutable (Arc), so the
    /// send helpers reach it through `&EventLoopState`. `new` installs a
    /// detached pool; the real loop replaces it with an endpoint-backed one.
    /// Independent of `linked_endpoints` by design (unicast can reach a
    /// non-neighbor). See [`crate::unicast`].
    pub unicast_pool: crate::unicast::UnicastPool,
    /// Which transport planes directed sends may use — per-session, so
    /// in-process nodes can each run a different policy (the CLI sources it
    /// from the hidden `--no-unicast`/`--no-gossip-directed`/`--no-circuit`
    /// flags, embed from its config). Read by [`crate::unicast::deliver`].
        /// Circuit routing table: the freshest link-state vector per origin, folded
    /// into the routing graph on demand. Populated from received `LinkState`
    /// frames; read by the circuit send path (and the topology view). See
    /// [`crate::circuit`].
    pub link_state: crate::circuit::LinkStateStore,
    /// Monotonic sequence for *our own* emitted link-state vectors, so peers keep
    /// the freshest and drop reorders.
    pub link_state_seq: u64,
    /// Locally-measured per-neighbour telemetry (ping RTT + delivery), keyed by
    /// endpoint id — the metrics we advertise for *our own* links in link-state.
    pub circuit_telemetry: HashMap<EndpointId, crate::circuit::NeighborProfile>,
    /// When `Some(deadline)` and not yet elapsed, the event loop runs
    /// a fast `beacon::ensure` burst (event-driven failover). Armed
    /// on `NeighborDown` — the beacon may have just died — so a
    /// survivor claims the freed rendezvous port in ~1s rather than
    /// waiting for the next 15s heal tick.
    pub reclaim_until: Option<Instant>,
    /// Recently-seen message ids, for duplicate suppression. Gossip
    /// (GRAFT/repair, topology churn, our own re-broadcasts, anti-entropy
    /// re-sends, the rendezvous double-path) can deliver the same message
    /// twice; `mark_seen` drops the repeat before it reaches the log /
    /// embed channel / agent. Bounded (`seen_ids_cap`, 2× the message log)
    /// so it always covers the retention window.
    pub seen: BoundedIdSet,
    /// User messages sent before we had a real-peer link (no gossip
    /// path yet — a bare `broadcast` would be a lost one-shot). Drained in
    /// FIFO order once `meshed` flips; a bounded FIFO queue (cap
    /// `PENDING_OUTBOUND_CAP`) so the backlog can't grow without limit.
    pub pending_outbound: BoundedQueue<Bytes>,
    pub state_file: Option<StateFile>,
    /// Whether the event loop is serving IPC yet. Starts `false` (the
    /// pre-loop state-file write reports "identity up, not yet serving");
    /// flipped `true` once the loop is draining, so a state-file reader
    /// (`agent-square ready`) can gate on a write that means the daemon will answer.
    pub ready: bool,
    /// When advertising (`create --advertise`), the directory's
    /// re-broadcast task reads the live participant count from here.
    /// Mirrors `participant_count` (`participants.len() + 1`), refreshed
    /// alongside every `write_participant_count`. `None` for the common
    /// non-advertising case (no shared counter to maintain).
    pub live_count: Option<Arc<AtomicUsize>>,
    pub message_log: MessageLog,
    /// Shard groups already reassembled + surfaced, so a body is surfaced
    /// once even if a shard is re-fetched (via anti-entropy) after its id aged
    /// out of [`seen`](Self::seen). Bounded — groups are rare and short-lived.
    pub reassembled_groups: BoundedFifoSet<ShardGroup>,
    /// Partial multipart bodies buffering toward reassembly — dedicated and
    /// byte-budgeted, decoupled from the message log so log eviction can
    /// never break reassembly. Swept for stale groups on the prune tick.
    pub reassembly: super::reassembly::ReassemblyStore,
    /// Serialized frames of our recent big (unlogged) outbound shard groups —
    /// the re-serve source for peers' `shard/repair` requests (big groups
    /// skip the message log, so anti-entropy can't heal them).
    pub shard_cache: super::reassembly::ShardCache,
    /// The `state` channel: an automerge CRDT plus the signed change frames that
    /// carried it (the re-serve store). Convergence and card authorization live
    /// in [`super::doc`]; reconciliation is heads-based anti-entropy. This is the
    /// document `agent-square state get` reads.
    pub state_doc: super::doc::MeshDoc,
    /// The `meta` channel's document — a second, independent shared-state channel.
    /// Gates foreign-card writes (unlike `state`), since a `meta` card carries a
    /// peer's cryptographic identity.
    pub meta_doc: super::doc::MeshDoc,
    /// Rolling start index for the **chat** anti-entropy digest window: each
    /// round advertises `message_log[digest_cursor ..]` (up to
    /// `ANTIENTROPY_DIGEST_MAX_IDS`), then advances/wraps so a log larger
    /// than one digest is swept over several rounds. (State/meta reconcile by
    /// automerge heads and need no cursor.)
    pub digest_cursor: usize,
    /// This member's signing identity (Ed25519). Shared with the
    /// send path so messages we author are signed before broadcast.
    /// The public key is the durable identity; the nickname is a
    /// non-unique display label (see `docs/history-integrity.md`).
    pub identity: Arc<Identity>,
    /// Our own per-author log cursor (Phase 2): `self_seq` is the next
    /// `Msg` sequence number to emit, `self_prev` the content hash of our
    /// last sent `Msg` (`None` until the first). Advanced on every send so
    /// our `Msg` stream is a `seq`+`prev` hash chain.
    pub self_seq: u64,
    pub self_prev: Option<String>,
    /// Phase 2 fork detection: per author pubkey (hex), the content hash
    /// seen at each `Msg` `seq`. A *different* hash at an already-seen
    /// `(pubkey, seq)` is cryptographic proof of equivocation → a `fork`
    /// event. Order-independent (gossip delivers out of order).
    pub author_seqs: HashMap<String, HashMap<u64, String>>,
    /// Author pubkeys already flagged as forked, so one `fork` event fires
    /// per offending key rather than per message.
    pub forked: HashSet<String>,
    /// Cross-author DAG (Phase 3): `by_hash` maps each known `Msg`'s content
    /// hash to its timestamp (for the `ts ≥ max(parents.ts)` backdating
    /// rule); `dag_heads` is the current tip set (hashes with no observed
    /// child) — the `parents` stamped on the next `Msg` we author. Both are
    /// pruned alongside the message-log eviction.
    pub by_hash: HashMap<String, i64>,
    pub dag_heads: HashSet<String>,
    /// When the last *verified* inbound gossip message arrived — the
    /// starvation watchdog's signal. Keyed on messages, not neighbor
    /// events: the roster-collapse showed links can look alive (or flap)
    /// while no traffic flows, and traffic is what membership feeds on.
    pub last_inbound_at: Instant,
    /// When the watchdog last ran a starvation recovery, if ever.
    /// Paired with `recovery_trips` to back off repeated attempts so a
    /// legitimately-last-survivor node settles into a sparse retry
    /// instead of warning every threshold period.
    pub last_recovery_at: Option<Instant>,
    /// Consecutive starvation recoveries without any inbound in between
    /// (drives the backoff; reset by `note_inbound`).
    pub recovery_trips: u32,
    /// `meshed` was cleared by a fault path (starvation recovery / hard
    /// resume edge) rather than never having been set. Gates
    /// `note_inbound`'s meshed-restore so a fresh joiner's pre-mesh
    /// inbound never flips `meshed` early; only a degraded node heals
    /// on traffic. See [`Self::note_degraded`].
    pub degraded: bool,
    /// Latch for the warn-only resident-memory leak signal: set once this
    /// process's resident memory first crosses `tuning::resident_memory_warn_mb`,
    /// so the `warn` fires exactly once per process rather than every prune
    /// tick. Purely observability — never gates behavior (see
    /// `timers::tick_prune`).
    pub resident_memory_warned: bool,
    /// Active `agent-square ping` round, if one is in flight. Armed by the
    /// `Ping` IPC command, filled by inbound `Pong`s, and finalized
    /// into a `ping_report` when its `deadline` elapses. One at a time:
    /// a fresh ping replaces any in-flight round. Boxed to keep the
    /// rarely-set round off the hot event-loop future's stack size.
    pub ping_round: Option<Box<PingRound>>,
    /// The raw mesh password when the mesh is password-protected (`None`
    /// otherwise). Handed to the blob server on its first offload so every
    /// blob ticket inherits the same password — a scraped ticket then can't be
    /// redeemed without it. Kept out of logs (`Password` redacts its `Debug`).
    pub mesh_password: Option<crate::protocol::crypto::Password>,
    /// The invite-only **creator's** mesh (secrets and all), retained so the
    /// `invite` IPC command can mint. `Some` only on the creator; set from
    /// [`EventLoopConfig`] after construction, so `new` defaults it to `None`
    /// and the many test call sites need no new argument.
    pub mint_mesh: Option<Mesh>,
    /// Symmetric key for broadcast-chat (`A2aMsg`) bodies on a passworded
    /// mesh, `derive_secret(mesh_key, "broadcast")` — domain-separated from
    /// the state/meta doc keys. `None` ⇒ chat stays plaintext. Wiped on drop.
    pub broadcast_key: Option<zeroize::Zeroizing<[u8; 32]>>,
}

/// An in-flight RTT round. `t1` is when the probe was broadcast;
/// `pongs` records each peer's local arrival instant so RTT is
/// `arrival - t1`; the round is emitted and cleared at `deadline`.
#[derive(Debug)]
pub struct PingRound {
    pub t1: TokioInstant,
    pub deadline: TokioInstant,
    pub pongs: HashMap<Nickname, TokioInstant>,
    /// When set (the embed/MCP `ping` request), the finalized RTT rows are
    /// delivered here instead of only emitted as a `ping_report` event — the
    /// in-process driver has no event stream to read the report from. `None`
    /// for the CLI/IPC path, which consumes the event.
    pub resp: Option<tokio::sync::oneshot::Sender<Vec<crate::gossip::event::PingRtt>>>,
}

impl EventLoopState {
    /// Build a fresh event-loop state. `now` is passed explicitly so
    /// tests can pin a deterministic instant.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "mesh_key is taken by value so the stretched key is dropped (zeroized) here once the per-channel keys are derived, rather than lingering in the caller"
    )]
    pub(crate) fn new(
        state_file: Option<StateFile>,
        now: Instant,
        identity: Arc<Identity>,
        mesh_password: Option<crate::protocol::crypto::Password>,
        mesh_key: Option<zeroize::Zeroizing<[u8; 32]>>,
    ) -> Self {
        // Per-channel encryption keys, domain-separated from each other and from
        // every other seed-derived secret. `None` (passwordless) ⇒ the docs and
        // broadcast chat stay plaintext, exactly as before.
        let state_key = mesh_key.as_deref().map(|key| {
            zeroize::Zeroizing::new(crate::protocol::crypto::derive_secret(key, b"state-doc"))
        });
        let meta_key = mesh_key.as_deref().map(|key| {
            zeroize::Zeroizing::new(crate::protocol::crypto::derive_secret(key, b"meta-doc"))
        });
        let broadcast_key = mesh_key.as_deref().map(|key| {
            zeroize::Zeroizing::new(crate::protocol::crypto::derive_secret(key, b"broadcast"))
        });
        Self {
            linked_endpoints: HashSet::new(),
            known_endpoints: BoundedFifoSet::new(KNOWN_ENDPOINTS_CAP),
            relink: Cooldown::new(RELINK_COOLDOWN),
            peerinfo: Cooldown::new(RELINK_COOLDOWN),
            participants: HashSet::new(),
            last_seen: HashMap::new(),
            participant_endpoints: HashMap::new(),
            rendezvous_id: None,
            quiet: BoundedFifoSet::new(QUIET_CAP),
            quiet_since: HashMap::new(),
            surfaced: HashSet::new(),
            last_sent_at: now,
            joined_at: crate::util::clock::unix_secs(),
            gossip_open: true,
            rendezvous_linked: false,
            announced: false,
            meshed: false,
            transport: crate::transport::TransportPolicy::DEFAULTS,
            unicast_pool: crate::unicast::UnicastPool::disconnected(),
            link_state: crate::circuit::LinkStateStore::default(),
            link_state_seq: 0,
            circuit_telemetry: HashMap::new(),
            reclaim_until: None,
            seen: BoundedIdSet::new(seen_ids_cap()),
            pending_outbound: BoundedQueue::new(PENDING_OUTBOUND_CAP),
            state_file,
            ready: false,
            live_count: None,
            message_log: MessageLog::new(message_log_size()),
            reassembled_groups: BoundedFifoSet::new(message_log_size()),
            reassembly: super::reassembly::ReassemblyStore::default(),
            shard_cache: super::reassembly::ShardCache::default(),
            state_doc: super::doc::MeshDoc::new(false).with_key(state_key),
            meta_doc: super::doc::MeshDoc::new(true).with_key(meta_key),
            digest_cursor: 0,
            identity,
            self_seq: 0,
            self_prev: None,
            author_seqs: HashMap::new(),
            forked: HashSet::new(),
            by_hash: HashMap::new(),
            dag_heads: HashSet::new(),
            last_inbound_at: now,
            last_recovery_at: None,
            recovery_trips: 0,
            degraded: false,
            resident_memory_warned: false,
            ping_round: None,
            mesh_password,
            mint_mesh: None,
            broadcast_key,
        }
    }

    /// Write `participant_count = participants.len() + 1` (we add 1
    /// for self) to the state file, if configured, carrying the current
    /// `ready` flag, and mirror the count into the advertise counter, if
    /// present. No-op for neither.
    pub(crate) fn write_participant_count(&self) {
        let count = self.participants.len() + 1;
        if let Some(sf) = self.state_file.as_ref() {
            sf.write(count, self.ready);
        }
        if let Some(live) = self.live_count.as_ref() {
            live.store(count, Ordering::Relaxed);
        }
    }

    /// Snapshot the live roster (active participants + quiet evictees),
    /// sorted most-recently-seen first. Backs `agent-square peers`, the MCP
    /// `mesh_info` roster, and the task sender's target picker /
    /// nickname validation.
    pub fn roster_snapshot(&self) -> RosterSnapshot {
        let now = Instant::now();
        let secs_since = |seen: &Instant| now.duration_since(*seen).as_secs();
        let mut participants: Vec<RosterEntry> = self
            .participants
            .iter()
            .map(|nick| RosterEntry {
                nickname: nick.clone(),
                // Active peers: their live `last_seen`.
                last_seen_secs_ago: self.last_seen.get(nick).map(secs_since),
                quiet: false,
                reach: self.reach_of(nick),
                transport: crate::unicast::lane_for(nick, self),
            })
            .chain(self.quiet.iter().map(|nick| RosterEntry {
                nickname: nick.clone(),
                // Quiet peers: their last-heard instant, retained in
                // `quiet_since` (the eviction drops `last_seen`). A quiet
                // peer has no live link, so it is always `Gossip`.
                last_seen_secs_ago: self.quiet_since.get(nick).map(secs_since),
                quiet: true,
                reach: Reach::Gossip,
                transport: crate::unicast::lane_for(nick, self),
            }))
            .collect();
        // Most-recently-seen first; unknown recency (no heartbeat yet) sorts last.
        participants.sort_by_key(|entry| entry.last_seen_secs_ago.unwrap_or(u64::MAX));
        RosterSnapshot {
            participants,
            count: self.participants.len() + 1,
        }
    }

    /// `Direct` only when this nickname's last self-advertised endpoint is a
    /// live link right now; otherwise `Gossip`. A live link is a neighbor in
    /// `linked_endpoints`, or the rendezvous itself when we're linked to it —
    /// the beacon gossips *as* the rendezvous, so a joiner's only link to the
    /// beacon is that one. A peer we haven't yet seen a `PeerInfo` from (or
    /// whose link dropped) reads `Gossip` and flips to `Direct` once its next
    /// advertisement lands.
    fn reach_of(&self, nick: &Nickname) -> Reach {
        let linked = self
            .participant_endpoints
            .get(nick)
            .is_some_and(|endpoint| {
                self.linked_endpoints.contains(endpoint)
                    || (self.rendezvous_id == Some(*endpoint) && self.rendezvous_linked)
            });
        if linked { Reach::Direct } else { Reach::Gossip }
    }

    /// `true` if we re-dialed + re-flooded `peer` within the last
    /// `RELINK_COOLDOWN_SECS` of `now`, so the caller should skip re-linking
    /// it again. Breaks the flap → re-dial → re-flood loop that otherwise
    /// turns one unstable peer into a mesh-wide CPU storm.
    pub(crate) fn relink_on_cooldown(&self, peer: EndpointId, now: Instant) -> bool {
        self.relink.on_cooldown(peer, now)
    }

    /// Record a re-link of `peer` at `now`.
    pub(crate) fn note_relink(&mut self, peer: EndpointId, now: Instant) {
        self.relink.note(peer, now);
    }

    /// `true` if a `NeighborUp` for `peer` already made us re-flood our own
    /// `PeerInfo` within the cooldown window of `now`, so the caller should
    /// skip re-flooding again. This is what stops a flapping link from
    /// re-broadcasting our address to the whole mesh on *every* up-transition
    /// (see `peerinfo`); a genuinely new neighbor, having no entry, still gets
    /// exactly one re-flood.
    pub(crate) fn peerinfo_on_cooldown(&self, peer: EndpointId, now: Instant) -> bool {
        self.peerinfo.on_cooldown(peer, now)
    }

    /// Record a `PeerInfo` re-flood triggered by `peer` at `now`.
    pub(crate) fn note_peerinfo(&mut self, peer: EndpointId, now: Instant) {
        self.peerinfo.note(peer, now);
    }

    /// Record `message` as seen and report whether it was *already* seen.
    /// `true` => this is a duplicate delivery the caller must drop. Keys on
    /// the author-bound [`Message::dedup_key`], so a forgery reusing a
    /// victim's id hashes to a different key and cannot suppress the genuine
    /// message. Delegates to the bounded [`BoundedIdSet`].
    pub(crate) fn mark_seen(&mut self, message: &Message) -> bool {
        self.seen.mark(message.dedup_key())
    }

    /// Try to reassemble the sharded body the `trigger` shard belongs to:
    /// buffer it in the dedicated [`reassembly`](Self::reassembly) store and,
    /// when it completes its group (and the group has not been surfaced
    /// before), return the synthesized logical [`Message`] — a surfacing view
    /// whose `id` is the group, so sender and receivers name it alike.
    ///
    /// Small (logged) groups get a second completion path off the message
    /// log: their shards are all retained there, so a shard healed by
    /// anti-entropy *after* the store's stale-group TTL reaped the partial
    /// buffer still completes the body — dedup (`seen`) means the healed
    /// shard is the only one re-ingested, and the store alone would sit on
    /// 1-of-N forever.
    pub(crate) fn reassemble(&mut self, trigger: &Message) -> Option<Message> {
        let shard = trigger.shard.as_ref()?;
        if self.reassembled_groups.contains(&shard.group) {
            return None;
        }
        match self.reassembly.ingest(trigger, Instant::now()) {
            super::reassembly::ShardIngest::Complete(logical) => {
                self.reassembled_groups.insert(shard.group.clone());
                Some(*logical)
            }
            super::reassembly::ShardIngest::Buffered
            | super::reassembly::ShardIngest::Dropped => {
                let logical = self.reassemble_from_log(trigger)?;
                // The store may still hold a partial buffer for this group
                // (e.g. just the trigger, post-TTL) — release its budget.
                self.reassembly.forget(&trigger.pubkey, &shard.group);
                self.reassembled_groups.insert(shard.group.clone());
                Some(logical)
            }
        }
    }

    /// The log-backed completion path for a **small** (logged) group: scan
    /// the message log for this author's shards and synthesize when every
    /// slot is present. Mirrors the store's synthesis exactly.
    fn reassemble_from_log(&self, trigger: &Message) -> Option<Message> {
        let shard = trigger.shard.as_ref()?;
        if !crate::protocol::message::shard_fits_log(trigger) {
            return None;
        }
        let slots = self
            .message_log
            .collect_shards(&shard.group, &trigger.pubkey, shard.total);
        if slots.iter().any(Option::is_none) {
            return None; // a shard is still missing
        }
        let mut body = String::new();
        for slot in &slots {
            body.push_str(slot.expect("every slot filled above").body.as_str());
        }
        // The concatenation of valid bodies is itself valid (no new control
        // chars), but fail closed rather than surface a malformed body.
        let body = crate::protocol::MessageBody::new(body).ok()?;
        let envelope = slots[0].expect("shard 0 present").clone();
        Some(super::reassembly::synthesize_from(envelope, body, &shard.group))
    }

    /// Mark the mesh degraded: a fault path (starvation recovery, hard
    /// resume edge) cleared `meshed`, so outbound user content buffers in
    /// `pending_outbound` instead of broadcasting into a dead overlay.
    /// `note_inbound` undoes it on the first proof of live traffic.
    pub(crate) fn note_degraded(&mut self) {
        self.meshed = false;
        self.degraded = true;
    }

    /// Record a verified inbound gossip message: refresh the starvation
    /// watchdog's signal and disarm its backoff. Called *before* dedup —
    /// a duplicate delivery still proves the mesh carries traffic. Also
    /// the moment a *degraded* node turns healthy again: inbound traffic
    /// is proof of a live path, so `meshed` is restored (the caller
    /// flushes `pending_outbound` on the flip). A fresh joiner that has
    /// never meshed is NOT flipped here — pre-mesh, relayed traffic can
    /// arrive before the overlay can carry our outbound, so the first
    /// real-peer `NeighborUp` keeps that job. Returns `true` on the
    /// degraded→meshed edge.
    pub(crate) fn note_inbound(&mut self, now: Instant) -> bool {
        self.last_inbound_at = now;
        self.recovery_trips = 0;
        self.last_recovery_at = None;
        if self.degraded {
            self.degraded = false;
            self.meshed = true;
            return true;
        }
        false
    }

    /// Should the heal tick run a starvation recovery *now*? True only
    /// when this node has been part of a mesh (`announced`) and knows at
    /// least one real peer to re-dial, yet has received **nothing** for
    /// over `threshold` — the roster-collapse signature, keyed on
    /// traffic rather than the (fallible) link view. Repeated trips back
    /// off 1-2-4-8× so a genuinely-last-survivor node retries sparsely.
    /// Deliberately NOT gated on `meshed`: recovery clears `meshed`, so
    /// that gate would disarm the watchdog after a single attempt.
    pub(crate) fn starvation_due(&self, now: Instant, threshold: Duration) -> bool {
        if !self.announced || self.known_endpoints.is_empty() {
            return false;
        }
        if now.duration_since(self.last_inbound_at) <= threshold {
            return false;
        }
        let backoff = threshold.saturating_mul(1 << self.recovery_trips.min(3));
        self.last_recovery_at
            .is_none_or(|at| now.duration_since(at) > backoff)
    }

    /// Record that a starvation recovery ran at `now` (arms the backoff).
    pub(crate) fn note_recovery(&mut self, now: Instant) {
        self.recovery_trips = self.recovery_trips.saturating_add(1);
        self.last_recovery_at = Some(now);
    }

    /// Record a `Msg`'s `(pubkey, seq, content-hash)` for fork detection.
    /// Returns `true` **exactly once** per offending key — when a *different*
    /// content hash is first seen at an already-recorded `(pubkey, seq)`,
    /// which is cryptographic proof the author equivocated (signed two
    /// conflicting messages at one seq). Order-independent. The caller emits
    /// a `fork` event on `true`; the message itself is still processed.
    pub(crate) fn note_msg_seq(&mut self, pubkey: &str, seq: u64, hash: String) -> bool {
        let seen = self.author_seqs.entry(pubkey.to_owned()).or_default();
        match seen.get(&seq) {
            Some(existing) if *existing != hash => {} // conflict → fall through
            Some(_) => return false,                  // same message, already recorded
            None => {
                seen.insert(seq, hash);
                return false;
            }
        }
        // Equivocation: flag the key, returning true only on first detection.
        self.forked.insert(pubkey.to_owned())
    }

    /// The `parents` to stamp on the next `Msg` we author: the current DAG
    /// tips, sorted (deterministic) and capped to [`MAX_DAG_PARENTS`] to
    /// bound message size. Usually a single head on a quiet mesh.
    #[must_use]
    pub fn dag_parents(&self) -> Vec<String> {
        let mut heads: Vec<String> = self.dag_heads.iter().cloned().collect();
        heads.sort_unstable();
        heads.truncate(MAX_DAG_PARENTS);
        heads
    }

    /// Fold a content `Msg` (hash `hash`, `parents`, timestamp `ts`) into the
    /// DAG — for messages we receive *and* ones we author. Returns `true` if
    /// `ts` is **before** a known parent's timestamp (a backdating violation
    /// to flag). Updates `by_hash` and moves the tip set: the named parents
    /// are no longer tips, and `hash` becomes one. Unknown parents are left
    /// alone (the set converges via anti-entropy).
    pub fn note_dag(&mut self, hash: String, parents: &[String], ts: i64) -> bool {
        let mut backdated = false;
        for parent in parents {
            if let Some(&parent_ts) = self.by_hash.get(parent)
                && ts < parent_ts
            {
                backdated = true;
            }
            self.dag_heads.remove(parent);
        }
        self.by_hash.insert(hash.clone(), ts);
        self.dag_heads.insert(hash);
        backdated
    }

    /// Prune an evicted message's content hash from the DAG indexes, keeping
    /// `by_hash`/`dag_heads` bounded alongside the message-log `VecDeque`.
    pub fn forget_hash(&mut self, hash: &str) {
        self.by_hash.remove(hash);
        self.dag_heads.remove(hash);
    }

    /// Prune an evicted `Msg`'s `(pubkey, seq)` from the fork-detection
    /// index, bounding `author_seqs`/`forked` to the message-log window: an
    /// identity whose messages have all aged out drops off both maps (so a
    /// sybil that fires once and vanishes is forgotten).
    ///
    /// Only drops the slot when `hash` is the one we recorded for that
    /// `(pubkey, seq)`. A fork retains *two* messages at one `(pubkey, seq)`;
    /// evicting the twin we did **not** record must not clear the recorded
    /// evidence, or a later anti-entropy resend of the still-retained twin
    /// would re-detect the equivocation as a fresh first sighting.
    pub fn forget_msg_seq(&mut self, pubkey: &str, seq: u64, hash: &str) {
        if let Some(seqs) = self.author_seqs.get_mut(pubkey)
            && seqs.get(&seq).is_some_and(|recorded| recorded == hash)
        {
            seqs.remove(&seq);
            if seqs.is_empty() {
                self.author_seqs.remove(pubkey);
                self.forked.remove(pubkey);
            }
        }
    }
}

/// Max `parents` stamped on a `Msg` — bounds the wire size of the causal
/// links. A quiet mesh has one head; this only bites under heavy concurrency.
const MAX_DAG_PARENTS: usize = 16;

#[cfg(test)]
mod tests {
    use super::{
        Duration, EndpointId, EventLoopState, Instant, KNOWN_ENDPOINTS_CAP, Message, Nickname,
        QUIET_CAP, RELINK_COOLDOWN_SECS, Reach,
    };
    use crate::protocol::{MessageBody, MessageId, MeshId};

    fn nick(name: &str) -> Nickname {
        Nickname::new(name.to_owned()).expect("valid test nickname")
    }

    /// An unsigned chat message carrying `id` — enough to exercise
    /// `mark_seen`, which keys on `dedup_key()` (`SHA-256(pubkey ‖ id)`).
    fn msg_with_id(id: &MessageId) -> Message {
        let mut message = Message::new_app(
            &MeshId::from("💬test"),
            &nick("author-nick"),
            crate::protocol::AppTag::from("a2a_msg"),
            None,
            None,
            MessageBody::from("body"),
        );
        message.id = id.clone();
        message
    }

    fn fresh_state() -> EventLoopState {
        EventLoopState::new(
            None,
            Instant::now(),
            std::sync::Arc::new(crate::protocol::identity::Identity::generate()),
            None,
            None,
        )
    }

    #[test]
    fn note_msg_seq_detects_equivocation_once() {
        let mut state = fresh_state();
        let key = "alice-key";
        // First sighting at a seq → recorded, no fork.
        assert!(!state.note_msg_seq(key, 0, "hashA".into()));
        // Same message again (same hash) → no fork.
        assert!(!state.note_msg_seq(key, 0, "hashA".into()));
        // Different hash at the SAME seq → fork, reported exactly once.
        assert!(
            state.note_msg_seq(key, 0, "hashB".into()),
            "conflict is a fork"
        );
        assert!(
            !state.note_msg_seq(key, 0, "hashC".into()),
            "already flagged: one event per key"
        );
    }

    #[test]
    fn note_msg_seq_is_per_key_and_per_seq() {
        let mut state = fresh_state();
        assert!(!state.note_msg_seq("alice", 0, "h0".into()));
        // A new seq for the same author is just the next entry, not a fork.
        assert!(!state.note_msg_seq("alice", 1, "h1".into()));
        // A different key reusing seq 0 with its own hash is independent.
        assert!(!state.note_msg_seq("bob", 0, "other".into()));
        // But a conflict on alice's seq 0 still fires.
        assert!(state.note_msg_seq("alice", 0, "h0-prime".into()));
    }

    #[test]
    fn forget_msg_seq_prunes_author_and_fork_flag() {
        let mut state = fresh_state();
        state.note_msg_seq("alice", 0, "h0".into());
        assert!(state.note_msg_seq("alice", 0, "h0-prime".into()), "fork");
        assert!(state.forked.contains("alice"));
        // Evicting the *recorded* twin (hash "h0") drops alice from both maps.
        state.forget_msg_seq("alice", 0, "h0");
        assert!(!state.author_seqs.contains_key("alice"));
        assert!(
            !state.forked.contains("alice"),
            "fork flag pruned with author"
        );
    }

    #[test]
    fn forget_msg_seq_keeps_evidence_when_other_twin_evicted() {
        // A fork keeps two twins at one (pubkey, seq). Evicting the twin we did
        // NOT record must not clear the recorded slot / fork flag — otherwise a
        // later resend of the still-retained twin re-detects the fork as new.
        let mut state = fresh_state();
        state.note_msg_seq("alice", 0, "recorded".into());
        assert!(state.note_msg_seq("alice", 0, "twin".into()), "fork fires");
        assert!(state.forked.contains("alice"));
        // Evict the non-recorded twin ("twin"): the slot for "recorded" stays.
        state.forget_msg_seq("alice", 0, "twin");
        assert!(
            state.author_seqs.contains_key("alice"),
            "recorded slot retained"
        );
        assert!(state.forked.contains("alice"), "fork flag retained");
        // A resend of the recorded twin does not re-fire the fork (already
        // flagged), proving the evidence survived the eviction.
        assert!(!state.note_msg_seq("alice", 0, "twin".into()));
    }

    #[test]
    fn note_dag_moves_tip_set() {
        let mut state = fresh_state();
        // First message (no parents) becomes the sole tip.
        assert!(!state.note_dag("h1".into(), &[], 100));
        assert_eq!(state.dag_parents(), vec!["h1".to_string()]);
        // A child referencing h1 makes h1 no longer a tip; h2 is the new one.
        assert!(!state.note_dag("h2".into(), &["h1".to_string()], 101));
        assert_eq!(state.dag_parents(), vec!["h2".to_string()]);
    }

    #[test]
    fn note_dag_keeps_concurrent_tips() {
        let mut state = fresh_state();
        state.note_dag("a".into(), &[], 1);
        state.note_dag("b".into(), &[], 1); // concurrent: no shared parent
        assert_eq!(state.dag_parents(), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn note_dag_flags_backdating() {
        let mut state = fresh_state();
        state.note_dag("parent".into(), &[], 200);
        // Child claims an earlier timestamp than a parent it references.
        assert!(state.note_dag("child".into(), &["parent".to_string()], 100));
        // A forward timestamp is fine.
        assert!(!state.note_dag("ok".into(), &["parent".to_string()], 300));
    }

    #[test]
    fn forget_hash_prunes_dag() {
        let mut state = fresh_state();
        state.note_dag("h".into(), &[], 1);
        state.forget_hash("h");
        assert!(state.dag_parents().is_empty());
    }

    /// A valid (curve-point) `EndpointId` derived deterministically from
    /// a seed — `EndpointId::from_bytes` rejects arbitrary bytes.
    fn endpoint_id(seed: u8) -> EndpointId {
        iroh::SecretKey::from_bytes(&[seed; 32]).public()
    }

    // The `known_endpoints` / `quiet` fields are `BoundedFifoSet`s wired with
    // their caps — the generic FIFO/dedup/remove behavior is covered in
    // `bounded_fifo_set`; these only assert the state wires the right cap.
    #[test]
    fn known_endpoints_field_is_capped() {
        let mut state = fresh_state();
        let first = endpoint_id(0);
        state.known_endpoints.insert(first);
        for index in 0..KNOWN_ENDPOINTS_CAP {
            state
                .known_endpoints
                .insert(endpoint_id(u8::try_from(index + 1).unwrap()));
        }
        assert!(state.known_endpoints.len() <= KNOWN_ENDPOINTS_CAP);
        assert!(
            !state.known_endpoints.contains(&first),
            "oldest evicted past the cap"
        );
    }

    #[test]
    fn roster_reach_tags_only_live_links_direct() {
        let mut state = fresh_state();
        let linked_ep = endpoint_id(1);
        let stale_ep = endpoint_id(2);
        // A participant whose advertised endpoint is a live link → direct.
        state.participants.insert(nick("linked"));
        state
            .participant_endpoints
            .insert(nick("linked"), linked_ep);
        state.linked_endpoints.insert(linked_ep);
        // Advertised, but the endpoint is not (any longer) a live link → gossip.
        state.participants.insert(nick("unlinked"));
        state
            .participant_endpoints
            .insert(nick("unlinked"), stale_ep);
        // No PeerInfo seen yet → no binding → gossip.
        state.participants.insert(nick("unknown"));
        // Quiet evictees are never live-linked → gossip.
        state.quiet.insert(nick("quiet"));

        let snapshot = state.roster_snapshot();
        let reach = |name: &str| {
            snapshot
                .participants
                .iter()
                .find(|entry| entry.nickname.as_str() == name)
                .unwrap_or_else(|| panic!("{name} missing from roster"))
                .reach
        };
        assert_eq!(reach("linked"), Reach::Direct);
        assert_eq!(reach("unlinked"), Reach::Gossip);
        assert_eq!(reach("unknown"), Reach::Gossip);
        assert_eq!(reach("quiet"), Reach::Gossip);
    }

    #[test]
    fn roster_reach_counts_the_rendezvous_link_for_the_beacon() {
        // The beacon gossips *as* the rendezvous, so a joiner's only link to
        // it is the rendezvous link (kept out of `linked_endpoints`). The
        // beacon's `PeerInfo` advertises the rendezvous endpoint, so the
        // roster must still tag it `direct` while we're rendezvous-linked.
        let mut state = fresh_state();
        let rendezvous_ep = endpoint_id(7);
        state.rendezvous_id = Some(rendezvous_ep);
        state.participants.insert(nick("beacon"));
        state
            .participant_endpoints
            .insert(nick("beacon"), rendezvous_ep);

        let reach = |current: &EventLoopState| {
            current
                .roster_snapshot()
                .participants
                .iter()
                .find(|entry| entry.nickname.as_str() == "beacon")
                .expect("beacon in roster")
                .reach
        };

        state.rendezvous_linked = true;
        assert_eq!(reach(&state), Reach::Direct, "linked rendezvous → direct");

        state.rendezvous_linked = false;
        assert_eq!(reach(&state), Reach::Gossip, "rendezvous down → gossip");
    }

    #[test]
    fn roster_transport_mirrors_the_send_decision() {
        use crate::unicast::Lane;
        let mut state = fresh_state();
        state.meshed = true;
        // Known endpoint while meshed → unicast (the send path would dial it).
        state.participants.insert(nick("dialable"));
        state
            .participant_endpoints
            .insert(nick("dialable"), endpoint_id(1));
        // No PeerInfo yet → nothing to dial or route a circuit to → gossip.
        state.participants.insert(nick("unknown"));

        let lane = |current: &EventLoopState, name: &str| {
            current
                .roster_snapshot()
                .participants
                .iter()
                .find(|entry| entry.nickname.as_str() == name)
                .unwrap_or_else(|| panic!("{name} missing from roster"))
                .transport
        };
        assert_eq!(lane(&state, "dialable"), Lane::Unicast);
        assert_eq!(lane(&state, "unknown"), Lane::Gossip);

        // Unmeshed: the known endpoint is no longer a unicast route, so the
        // circuit plane picks it up.
        state.meshed = false;
        assert_eq!(lane(&state, "dialable"), Lane::Circuit);

        // Policy narrows the column exactly like it narrows `deliver`.
        state.transport.circuit = false;
        assert_eq!(lane(&state, "dialable"), Lane::Gossip);
        state.transport.gossip_directed = false;
        assert_eq!(lane(&state, "dialable"), Lane::Unreachable);
    }

    #[test]
    fn quiet_field_is_capped() {
        // `quiet` is drained only on return, so the cap is what stops a churn /
        // sybil stream of one-shot nicknames from growing it without bound.
        let mut state = fresh_state();
        for index in 0..(QUIET_CAP + 5) {
            state.quiet.insert(nick(&format!("ghost-{index}")));
        }
        assert!(
            state.quiet.len() <= QUIET_CAP,
            "quiet stays capped under churn: {}",
            state.quiet.len()
        );
    }

    #[test]
    fn mark_seen_delegates_dedup() {
        // Smoke test the `EventLoopState` → `BoundedIdSet` delegation; the
        // bounded-FIFO/eviction logic itself is covered in `bounded_id_set`.
        let mut state = fresh_state();
        let message = msg_with_id(&MessageId::random());
        assert!(
            !state.mark_seen(&message),
            "first sighting is not a duplicate"
        );
        assert!(state.mark_seen(&message), "second sighting is a duplicate");
    }

    #[test]
    fn mark_seen_keys_on_pubkey_not_id() {
        // Two messages sharing an id but signed by different keys must NOT
        // dedup against each other — the fix for the id-collision suppression
        // attack. Distinct pubkeys ⇒ distinct `dedup_key`s.
        let mut state = fresh_state();
        let id = MessageId::random();
        let mut victim = msg_with_id(&id);
        victim.pubkey = "aa".repeat(32);
        let mut attacker = msg_with_id(&id);
        attacker.pubkey = "bb".repeat(32);
        assert!(!state.mark_seen(&victim), "victim's message is fresh");
        assert!(
            !state.mark_seen(&attacker),
            "an id-reusing message under a different key must not be deduped as the victim's"
        );
        assert!(state.mark_seen(&victim), "victim's own resend is deduped");
    }

    // Replicates the mesh-wide CPU runaway at its mechanism: a peer whose
    // transport link flaps (each `NeighborDown` drops it from
    // `linked_endpoints`, each re-learned `PeerInfo` re-passes the novelty
    // gate) must NOT re-dial + re-flood once per flap. See memory
    // `cpu-runaway-membership-amplifier`.
    #[test]
    fn relink_cooldown_caps_a_flapping_peer() {
        let peer = endpoint_id(7);
        let start = Instant::now();

        // Baseline — documents the BUG: the bare novelty gate (remove+insert)
        // re-links on EVERY flap, i.e. unbounded amplification.
        let mut bare_state = fresh_state();
        let mut bare = 0;
        for _ in 0..100u32 {
            bare_state.linked_endpoints.remove(&peer);
            if bare_state.linked_endpoints.insert(peer) {
                bare += 1;
            }
        }
        assert_eq!(
            bare, 100,
            "without a cooldown every flap re-links — the runaway"
        );

        // With the cooldown: 100 flaps inside one window collapse to ONE re-link.
        let mut cooled = fresh_state();
        let mut relinks = 0;
        for index in 0..100u32 {
            let now = start + Duration::from_millis(u64::from(index) * 10);
            cooled.linked_endpoints.remove(&peer);
            if !cooled.relink_on_cooldown(peer, now) && cooled.linked_endpoints.insert(peer) {
                cooled.note_relink(peer, now);
                relinks += 1;
            }
        }
        assert_eq!(relinks, 1, "the cooldown caps re-links to once per window");

        // Past the window a genuine re-link is allowed again (no permanent lockout).
        let later = start + Duration::from_secs(RELINK_COOLDOWN_SECS + 1);
        assert!(!cooled.relink_on_cooldown(peer, later));
    }

    // The residual flap amplifier the re-link cooldown did NOT cover: every
    // `NeighborUp` re-floods our own `PeerInfo` to the whole mesh, and a
    // flapping link re-triggers `NeighborUp` on each up-transition, so without
    // a second cooldown one bad node re-floods the mesh ~once per flap (the
    // ~7.4k-per-host `neighbor up` storm seen in the distributed soak). The
    // PeerInfo cooldown collapses that to once per window per endpoint while
    // still letting a genuinely new neighbor get exactly one re-flood.
    #[test]
    fn peerinfo_cooldown_caps_a_flapping_peer() {
        let peer = endpoint_id(7);
        let start = Instant::now();

        // The pre-fix `NeighborUp` arm re-flooded unconditionally when
        // `announced`, so 100 flaps == 100 mesh-wide PeerInfo broadcasts. The
        // gate below replays the same 100 flaps and counts what now actually
        // re-floods.
        let mut state = fresh_state();
        let mut refloods = 0;
        for index in 0..100u32 {
            let now = start + Duration::from_millis(u64::from(index) * 10);
            if !state.peerinfo_on_cooldown(peer, now) {
                state.note_peerinfo(peer, now);
                refloods += 1;
            }
        }
        assert_eq!(
            refloods, 1,
            "the cooldown caps PeerInfo re-floods to one per window (was 100, one per flap)"
        );

        // A genuinely different neighbor in the same window still gets its own
        // re-flood (the choke targets the *flapping* endpoint, not all peers).
        let fresh_peer = endpoint_id(8);
        assert!(!state.peerinfo_on_cooldown(fresh_peer, start));

        // Past the window the flapping peer may re-flood once more (no permanent silence).
        let later = start + Duration::from_secs(RELINK_COOLDOWN_SECS + 1);
        assert!(!state.peerinfo_on_cooldown(peer, later));
    }

    // Under a long flap storm against a steady peer set, every collection *we*
    // own stays flat — so the soak's monotonic 4.6 GB resident-memory climb is provably
    // below our layer (the iroh transport), not in daemon state. This replays
    // the per-flap state mutations the `NeighborUp`/`NeighborDown` arms +
    // `handle_peer_info` make, for many flaps over simulated hours.
    #[test]
    fn app_state_stays_bounded_under_flap_churn() {
        let mut state = fresh_state();
        let start = Instant::now();
        // A small, *steady* roster (the soak's shape: ~10 members, ~7.4k flaps),
        // not a sybil endpoint stream — endpoints are real curve points.
        let peers: Vec<EndpointId> = (0..8u8).map(endpoint_id).collect();

        for index in 0..10_000u32 {
            let now = start + Duration::from_millis(u64::from(index) * 100);
            let peer = peers[index as usize % peers.len()];
            // NeighborUp → PeerInfo reflood gate + (via handle_peer_info) link.
            if !state.peerinfo_on_cooldown(peer, now) {
                state.note_peerinfo(peer, now);
            }
            if !state.relink_on_cooldown(peer, now) {
                state.note_relink(peer, now);
                state.linked_endpoints.insert(peer);
                state.known_endpoints.insert(peer);
            }
            // NeighborDown drops the transport link (kept in known_endpoints).
            state.linked_endpoints.remove(&peer);
        }

        // None of our collections grew with the 10k flaps: each is bounded by
        // the roster size or its hard cap, never by the flap count.
        assert!(
            state.relink.len() <= peers.len(),
            "relink flat: {}",
            state.relink.len()
        );
        assert!(
            state.peerinfo.len() <= peers.len(),
            "peerinfo flat: {}",
            state.peerinfo.len()
        );
        assert!(
            state.linked_endpoints.len() <= peers.len(),
            "linked_endpoints flat"
        );
        assert!(
            state.known_endpoints.len() <= KNOWN_ENDPOINTS_CAP.min(peers.len()),
            "known_endpoints bounded"
        );
    }

    #[test]
    fn relink_cooldown_is_per_peer_and_bounded() {
        let mut state = fresh_state();
        let start = Instant::now();
        let flapping = endpoint_id(7);
        let other = endpoint_id(8);

        // One peer's cooldown never gates a different peer.
        state.note_relink(flapping, start);
        assert!(state.relink_on_cooldown(flapping, start));
        assert!(!state.relink_on_cooldown(other, start));

        // Many distinct peers re-linked across more than a full window:
        // expired entries are pruned, so the map never accumulates them all.
        for seed in 0..50u8 {
            let when = start + Duration::from_secs(u64::from(seed));
            state.note_relink(endpoint_id(seed), when);
        }
        assert!(
            state.relink.len() <= usize::try_from(RELINK_COOLDOWN_SECS).unwrap() + 1,
            "expired cooldown entries are pruned (map stays bounded)"
        );
    }

    #[test]
    fn dedup_window_covers_the_retention_window() {
        use crate::util::tuning::{message_log_size, seen_ids_cap};

        // Invariant the whole design rests on: the dedup set must outlive
        // the message log. Anti-entropy re-broadcasts any message still in
        // the log; if its id had scrolled out of the dedup set the resend
        // would be reprocessed and **re-surfaced** to the agent.
        assert!(
            seen_ids_cap() >= message_log_size(),
            "dedup cap ({}) must cover the retention window ({})",
            seen_ids_cap(),
            message_log_size(),
        );

        let mut state = fresh_state();
        let earliest = msg_with_id(&MessageId::random());
        assert!(!state.mark_seen(&earliest));
        // Mark a full buffer's worth of later messages.
        for _ in 1..message_log_size() {
            state.mark_seen(&msg_with_id(&MessageId::random()));
        }
        // The earliest is still within the dedup window, so an anti-entropy
        // resend of it (while it is still retained in the log) is dropped as
        // a duplicate rather than surfaced a second time.
        assert!(
            state.mark_seen(&earliest),
            "a resend of a still-retained message must be deduped, not re-surfaced"
        );
    }

    const STARVE: Duration = Duration::from_secs(10);

    // The watchdog must only ever fire for a node that (a) was part of a
    // mesh and (b) knows a real peer to re-dial — a lone creator is alone
    // by construction, not starved.
    #[test]
    fn starvation_due_requires_announce_known_peers_and_silence() {
        let mut state = fresh_state();
        let past_threshold = Instant::now() + STARVE + Duration::from_secs(1);
        assert!(
            !state.starvation_due(past_threshold, STARVE),
            "never announced => never starved"
        );
        state.announced = true;
        assert!(
            !state.starvation_due(past_threshold, STARVE),
            "no known peers => alone, not starved"
        );
        state.known_endpoints.insert(endpoint_id(1));
        assert!(state.starvation_due(past_threshold, STARVE));
        // Fresh inbound disarms it until the threshold passes again.
        state.note_inbound(past_threshold);
        assert!(!state.starvation_due(past_threshold, STARVE));
        assert!(state.starvation_due(past_threshold + STARVE + Duration::from_secs(1), STARVE));
    }

    #[test]
    fn starvation_recovery_backs_off_then_resets_on_inbound() {
        let mut state = fresh_state();
        state.announced = true;
        state.known_endpoints.insert(endpoint_id(1));
        let first = Instant::now() + STARVE + Duration::from_secs(1);
        assert!(state.starvation_due(first, STARVE));
        state.note_recovery(first);
        // One trip: the next attempt waits 2x the threshold, not 1x.
        assert!(!state.starvation_due(first + STARVE + Duration::from_secs(1), STARVE));
        let second = first + (STARVE * 2) + Duration::from_secs(1);
        assert!(state.starvation_due(second, STARVE));
        state.note_recovery(second);
        // Two trips: 4x.
        assert!(!state.starvation_due(second + (STARVE * 2) + Duration::from_secs(1), STARVE));
        assert!(state.starvation_due(second + (STARVE * 4) + Duration::from_secs(1), STARVE));
        // Any inbound resets the backoff to the base threshold.
        let healed = second + (STARVE * 4) + Duration::from_secs(2);
        state.note_inbound(healed);
        assert_eq!(state.recovery_trips, 0);
        assert!(!state.starvation_due(healed + STARVE, STARVE));
        assert!(state.starvation_due(healed + STARVE + Duration::from_secs(1), STARVE));
    }

    #[test]
    fn starvation_backoff_caps_at_eight_threshold() {
        let mut state = fresh_state();
        state.announced = true;
        state.known_endpoints.insert(endpoint_id(1));
        let mut last_recovery = Instant::now() + STARVE + Duration::from_secs(1);
        // Trip well past the cap (`recovery_trips.min(3)` => 8x ceiling).
        for _ in 0..6 {
            state.note_recovery(last_recovery);
            last_recovery += STARVE * 8 + Duration::from_secs(1);
        }
        state.note_recovery(last_recovery);
        assert!(
            !state.starvation_due(last_recovery + STARVE * 8, STARVE),
            "inside the capped backoff window"
        );
        assert!(
            state.starvation_due(last_recovery + STARVE * 8 + Duration::from_secs(1), STARVE),
            "the backoff never grows past 8x the threshold"
        );
    }

    // `meshed` restoration on inbound is gated on the `degraded` fault
    // flag: a fresh joiner's pre-mesh inbound (relayed backlog) must not
    // flip `meshed` early — that stays the first real-peer NeighborUp's
    // job — while a degraded node heals on the first proof of traffic.
    #[test]
    fn note_inbound_restores_meshed_only_when_degraded() {
        let mut state = fresh_state();
        let now = Instant::now();
        assert!(
            !state.note_inbound(now),
            "pre-mesh inbound never flips meshed"
        );
        assert!(!state.meshed);
        state.meshed = true;
        assert!(!state.note_inbound(now), "healthy meshed: no edge");
        assert!(state.meshed);
        state.note_degraded();
        assert!(!state.meshed);
        assert!(
            state.note_inbound(now),
            "degraded node heals on first inbound (caller flushes)"
        );
        assert!(state.meshed);
        assert!(!state.degraded);
    }
}
