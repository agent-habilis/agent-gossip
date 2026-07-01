//! Behavioural knobs. Changing these affects timing, capacity, and
//! policy but never the on-the-wire format — the size cap belongs in
//! `protocol::message` (`MAX_MESSAGE_SIZE`).
//!
//! The poll/MCP buffer size (`DEFAULT_MESSAGE_LOG_SIZE`) lives in
//! `crate::util::consts` — it anchors the shared IPC response cap.

/// In-memory message-log capacity: how many recent messages each member
/// retains as the anti-entropy recovery source and poll/fetch history. A
/// bigger log lets a reconnecting peer recover a longer gap. Fixed at
/// [`crate::util::consts::DEFAULT_MESSAGE_LOG_SIZE`] (1000), clamped to `>= 1`; edit
/// the const to change it.
pub(crate) fn message_log_size() -> usize {
    crate::util::consts::DEFAULT_MESSAGE_LOG_SIZE.max(1)
}

/// How many recently-seen message ids are retained for duplicate
/// suppression. Kept at **2× the message log** so it always covers the
/// retention window with margin: anti-entropy resends any message still
/// in the log, and a resend whose id had scrolled out of this set would be
/// reprocessed and **re-surfaced**. Scales with `message_log_size()`.
pub(crate) fn seen_ids_cap() -> usize {
    message_log_size().saturating_mul(2)
}

/// How many outbound user messages are buffered while the node has no
/// gossip link yet (sent before the first `NeighborUp`). Flushed in
/// order once connected; oldest dropped past this cap so a node that
/// spams while offline can't grow memory unbounded.
pub(crate) const PENDING_OUTBOUND_CAP: usize = 64;

/// How many distinct peer endpoint ids we remember for the
/// rendezvous-independent re-bridge (`gossip::heal::rebridge_known`).
/// Survives `NeighborDown` (unlike `linked_endpoints`) so a node that
/// lost every link can still re-dial peers directly when the
/// rendezvous/relay is the bottleneck. Bounded FIFO (oldest evicted)
/// so long-lived swarms with churn can't grow it unbounded; sized well
/// above a typical swarm so recent peers are always retained.
pub(crate) const KNOWN_ENDPOINTS_CAP: usize = 64;

/// Anti-entropy: how often a member broadcasts its digest (recent
/// message ids it holds) so peers can re-send anything it missed
/// while partitioned/asleep. Short enough that a returning peer
/// recovers within a couple of cycles; digests are small and a
/// re-send only happens when there is an actual gap, so steady-state
/// cost is one tiny message per interval.
pub(crate) const ANTIENTROPY_INTERVAL_SECS: u64 = 10;

/// Max ids advertised per digest **window**. A digest carries up to two
/// windows: an **open-ended newest** one (`[lo, i64::MAX]`, which drives
/// reconnect recovery — holders re-send every *newer* message the sender
/// lacks) and a rolling **closed** older one (`[lo, hi]`, which reconciles
/// deep interior gaps without re-sending the out-of-window remainder). At
/// 70 ids each (~140 total) the body packs ids as raw 16-byte UUIDs
/// Base58-encoded (~22 chars/id) to ~3.1 KB; plus the `{windows:[…]}` and
/// message envelope (the `🐝…` id alone is ~80 chars) it stays under
/// `MAX_MESSAGE_SIZE` (3840) — guarded by the `digest_fits_gossip_cap`
/// test. Sized to a single gossip message, **not** the (larger,
/// configurable) log, which the rolling cursor sweeps across rounds.
pub(crate) const ANTIENTROPY_DIGEST_WINDOW_IDS: usize = 70;

/// Max messages re-broadcast in response to one received digest, so a
/// far-behind peer can't trigger an unbounded burst. This throttles
/// deep-backfill throughput (~`this × peers` messages per
/// `ANTIENTROPY_INTERVAL_SECS`). Default [`crate::util::consts::ANTIENTROPY_MAX_RESEND`];
/// hidden flag `--antientropy-max-resend` (tests raise it for deep backfill).
pub(crate) fn antientropy_max_resend() -> usize {
    current().antientropy_max_resend.max(1)
}

/// Default max direct peer connections (gossip relays beyond this)
pub(crate) const DEFAULT_MAX_DIRECT_PEERS: usize = 25;

/// HyParView active-view capacity — the full-mesh threshold that eliminates
/// membership churn (and the churn-driven leak) for swarms ≤ it. Default
/// [`crate::util::consts::GOSSIP_ACTIVE_VIEW_CAPACITY`] (32); hidden flag
/// `--active-view-capacity` — set it *small* to deliberately reproduce the
/// gossip-churn leak at any node count.
pub(crate) fn gossip_active_view_capacity() -> usize {
    current().gossip_active_view_capacity.max(1)
}

/// HyParView passive-view capacity (healing/shuffle contact pool). Default
/// [`crate::util::consts::GOSSIP_PASSIVE_VIEW_CAPACITY`] (64); hidden flag
/// `--passive-view-capacity`.
pub(crate) fn gossip_passive_view_capacity() -> usize {
    current().gossip_passive_view_capacity.max(1)
}

/// Capacity of the embed facade's inbound broadcast channel. Bounded
/// so a slow embedder never backpressures the gossip/membership loop;
/// under sustained lag the oldest buffered messages are dropped and
/// the consumer observes `RecvError::Lagged` (see `embed::SwarmSession`).
pub(crate) const EMBED_INBOUND_CAP: usize = 1024;

/// Soft resident-memory threshold (`MiB`) above which the daemon emits a
/// one-shot `warn` (log + JSON `info` event) on its slow prune tick — the
/// in-process leak-visibility signal the distributed soak lacked. **Warn-only**:
/// it never exits; host safety is the a2a runbook's OS resource caps. Fixed at
/// [`crate::util::consts::RESIDENT_MEMORY_WARN_MB`] (1024, well above a healthy
/// node's tens of `MiB`); `0` there disables it. Edit the const to tune.
pub(crate) fn resident_memory_warn_mb() -> u64 {
    crate::util::consts::RESIDENT_MEMORY_WARN_MB
}

/// How often an idle daemon broadcasts a `Presence::Alive` keepalive.
/// Active talkers never emit one — any sent gossip message resets the
/// timer, so chatty swarms pay zero heartbeat cost.
pub(crate) const ALIVE_INTERVAL_SECS: u64 = 30;

/// How long a peer can go unheard before the sweeper evicts it.
/// Must exceed `ALIVE_INTERVAL_SECS` comfortably — 3x absorbs one or
/// two lost gossip rounds. Worst-case ghost window is
/// `alive_timeout + sweep_interval`.
///
/// Default [`crate::util::consts::ALIVE_TIMEOUT_SECS`]; hidden flag
/// `--alive-timeout-secs` so integration tests exercise eviction in
/// seconds instead of minutes.
pub(crate) fn alive_timeout_secs() -> u64 {
    current().alive_timeout_secs
}

/// How often the sweeper walks `last_seen` looking for expired peers.
/// Bounds the maximum statusline staleness from a peer's true
/// disappearance to its eviction. Hidden flag `--sweep-interval-secs`.
pub(crate) fn sweep_interval_secs() -> u64 {
    current().sweep_interval_secs
}

/// Idle-debounce timeout for a task (seconds). Hidden flag
/// `--task-timeout-secs` so integration tests exercise eviction in seconds.
pub(crate) fn task_timeout_secs() -> u64 {
    current().task_timeout_secs
}

/// How often the ball-owner's daemon emits a task keepalive (seconds).
/// Hidden flag `--task-keepalive-secs`.
pub(crate) fn task_keepalive_secs() -> u64 {
    current().task_keepalive_secs
}

/// Grace before an **unmeshed joiner** co-hosts the rendezvous anyway
/// (empty swarm ⇒ become the beacon for the next joiner). Rationale:
/// `EventLoopConfig::cohost`. Non-blocking — only consulted
/// on heal ticks, never delays `ready`; a joiner that meshes co-hosts
/// the moment it has a neighbor, well before this. Hidden flag
/// `--beacon-cohost-grace-secs`.
pub(crate) fn cohost_grace_secs() -> u64 {
    current().cohost_grace_secs
}

/// How long an `ahsw ping` round collects pongs before the daemon
/// emits its `ping_report`. Long enough for a relayed round-trip
/// across the mesh; hidden flag `--ping-window-secs` so tests don't
/// wait the full window.
pub(crate) fn ping_window_secs() -> u64 {
    current().ping_window_secs
}

/// How often the CLI daemon re-reads its parent pid to detect orphaning.
/// Default [`crate::util::consts::PPID_WATCH_INTERVAL_MS`]; hidden flag
/// `--ppid-watch-interval-ms` so the subprocess test sees the self-exit in
/// milliseconds instead of the production seconds.
pub(crate) fn ppid_watch_interval_ms() -> u64 {
    current().ppid_watch_interval_ms.max(1)
}

/// Process tuning sourced **once** at daemon startup from the hidden CLI
/// flags (`--alive-timeout-secs`, …). Replaces the former env-var reads: an
/// experiment is now an edit-the-const + commit, and a subprocess test passes
/// the flag. Production runs on [`Tuning::DEFAULTS`] (the `crate::util::consts`
/// values) when [`init`] is never called (the embed / MCP path).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Tuning {
    pub alive_timeout_secs: u64,
    pub sweep_interval_secs: u64,
    pub task_timeout_secs: u64,
    pub task_keepalive_secs: u64,
    pub cohost_grace_secs: u64,
    pub ping_window_secs: u64,
    pub ppid_watch_interval_ms: u64,
    pub heal_stall_threshold_secs: u64,
    pub starvation_threshold_secs: u64,
    pub advertise_interval_secs: u64,
    pub directory_expiry_secs: u64,
    pub antientropy_max_resend: usize,
    pub directory_private: bool,
    pub gossip_active_view_capacity: usize,
    pub gossip_passive_view_capacity: usize,
}

impl Tuning {
    /// The production defaults, all from `crate::util::consts`.
    pub(crate) const DEFAULTS: Self = Self {
        alive_timeout_secs: crate::util::consts::ALIVE_TIMEOUT_SECS,
        sweep_interval_secs: crate::util::consts::SWEEP_INTERVAL_SECS,
        task_timeout_secs: crate::util::consts::TASK_TIMEOUT_SECS,
        task_keepalive_secs: crate::util::consts::TASK_KEEPALIVE_SECS,
        cohost_grace_secs: crate::util::consts::BEACON_COHOST_GRACE_SECS,
        ping_window_secs: crate::util::consts::PING_WINDOW_SECS,
        ppid_watch_interval_ms: crate::util::consts::PPID_WATCH_INTERVAL_MS,
        heal_stall_threshold_secs: crate::util::consts::HEAL_STALL_THRESHOLD_SECS,
        starvation_threshold_secs: crate::util::consts::STARVATION_THRESHOLD_SECS,
        advertise_interval_secs: crate::util::consts::ADVERTISE_INTERVAL_SECS,
        directory_expiry_secs: crate::util::consts::DIRECTORY_EXPIRY_SECS,
        antientropy_max_resend: crate::util::consts::ANTIENTROPY_MAX_RESEND,
        directory_private: false,
        gossip_active_view_capacity: crate::util::consts::GOSSIP_ACTIVE_VIEW_CAPACITY,
        gossip_passive_view_capacity: crate::util::consts::GOSSIP_PASSIVE_VIEW_CAPACITY,
    };
}

impl Default for Tuning {
    fn default() -> Self {
        Self::DEFAULTS
    }
}

static TUNING: std::sync::OnceLock<Tuning> = std::sync::OnceLock::new();

/// Install the process tuning, once, at daemon startup (from the parsed CLI
/// flags). A second call is ignored; if never called (embed / MCP), [`current`]
/// returns [`Tuning::DEFAULTS`].
pub(crate) fn init(tuning: Tuning) {
    let _ = TUNING.set(tuning);
}

fn current() -> Tuning {
    TUNING.get().copied().unwrap_or(Tuning::DEFAULTS)
}

/// How often the daemon re-asserts `participant_count` +
/// `last_updated` into the session state file even when membership is
/// unchanged. A fresh `last_updated` is what external readers (the
/// shell statusline) treat as liveness — file presence alone would
/// show a false pill after a hard crash. Coupled to the statusline's
/// staleness window, which must stay >= ~3x this value (currently 30s
/// for a 10s cadence); change both together.
pub(crate) const STATE_REFRESH_SECS: u64 = 10;

/// Cadence of the unconditional gossip healer (`gossip::heal::tick_heal`).
/// 15s balances fast re-mesh after a partition against steady-state
/// cost — one detached rendezvous connect-probe plus one HyParView
/// control message per tick when already healthy.
pub(crate) const HEAL_INTERVAL_SECS: u64 = 15;

/// Consecutive failed gossip-topic resubscribe attempts (one per heal
/// tick after the stream terminally ends) before the daemon gives up
/// and shuts down. A subscribe error means the gossip actor itself is
/// gone — endpoint closed, unrecoverable — so 8 (~2 min at the heal
/// cadence) is generosity, not hope; a deaf daemon must not pose as a
/// live member forever.
pub(crate) const RESUBSCRIBE_MAX_ATTEMPTS: u32 = 8;

/// Backoff bounds between failed IPC `accept`s. An accept error is
/// almost always transient (fd exhaustion under load, an aborted
/// handshake), so the listener retries forever instead of dying — the
/// backoff (doubling MIN→MAX, reset on any successful accept) just
/// keeps a persistently failing listener from spinning hot.
pub(crate) const IPC_ACCEPT_BACKOFF_MIN_MS: u64 = 100;
pub(crate) const IPC_ACCEPT_BACKOFF_MAX_SECS: u64 = 5;

/// Per-connection IPC I/O deadline: how long the daemon waits for a
/// connected client to send its command line, and for the response
/// write to complete. A client that connects and goes silent would
/// otherwise pin a task + fd for the daemon's lifetime; well above any
/// real `msg`/`poll` round-trip, so only a hung client ever hits it.
pub(crate) const IPC_IO_TIMEOUT_SECS: u64 = 10;

/// `ahsw ready` gate: how long to wait for the daemon's `--state-file` to
/// report `ready: true` before giving up (the `--timeout-secs` default),
/// and the fixed interval between file reads while waiting. 30s covers a
/// cold daemon start (the file appears sub-second once the process is up).
/// Client-side, so these are not part of the daemon `Tuning` struct.
pub(crate) const READY_MAX_SECS: u64 = 30;
pub(crate) const READY_POLL_INTERVAL_MS: u64 = 100;

/// How fresh a `ready: true` state-file write must be for the gate to trust
/// it. A live daemon rewrites the file every `STATE_REFRESH_SECS` (10s), so
/// a `last_updated` older than this window means the writer is gone — e.g. a
/// `ready: true` file left behind by a prior daemon killed with SIGKILL (which
/// skips the file-removing shutdown path). Two heartbeats of slack absorbs a
/// missed refresh without trusting a truly stale file.
pub(crate) const READY_FRESH_SECS: u64 = 2 * STATE_REFRESH_SECS;

/// Long-poll (`poll`/`fetch_messages` blocking mode): the server's hard clamp
/// on any caller's `wait_ms` — kept under typical MCP-host per-request timeouts
/// so a held call returns before the host gives up. The daemon never blocks on
/// a long-poll (the waiter parks in a registry); this only bounds how long a
/// *caller's* in-flight read is held. The recommended client/skill wait
/// (~15000 ms) is documented in the skills and the MCP `fetch_messages` tool,
/// not a const — callers pass an explicit value, the server only enforces this
/// ceiling. Client/policy timing, so not part of the daemon `Tuning` struct.
pub(crate) const LONGPOLL_MAX_MS: u64 = 60_000;

/// Upper bound on the healer's detached rendezvous connect-probe.
/// Generous enough to absorb a public relay/lookup warmup after a
/// real network change, capped well under `HEAL_INTERVAL_SECS` so at
/// most one probe task is ever outstanding.
pub(crate) const HEAL_PROBE_SECS: u64 = 5;

/// Probe budget for the resume-edge hard heal. Longer than
/// `HEAL_PROBE_SECS` because a cold relay re-home after a freeze
/// routinely exceeds the steady-state 5s; the path is rare so a probe
/// that briefly outlives one heal interval (still detached) is fine.
pub(crate) const HEAL_HARD_PROBE_SECS: u64 = 20;

/// A heal inter-tick gap above this many seconds means the process was
/// frozen between ticks (App Nap / coalescing / sleep) and must hard
/// re-bootstrap. Default [`crate::util::consts::HEAL_STALL_THRESHOLD_SECS`]
/// (60s) — safely above `HEAL_INTERVAL_SECS` (15s) so normal slack never
/// trips it. Hidden flag `--heal-stall-threshold-secs` so subprocess tests
/// drive it in seconds.
pub(crate) fn heal_stall_threshold_secs() -> u64 {
    current().heal_stall_threshold_secs
}

/// No verified inbound gossip for this long, while real peers are known,
/// trips the heal arm's starvation watchdog (re-bridge + re-announce; see
/// `gossip::heal::recover_from_starvation`). Keyed on traffic, not the link
/// view — links can look alive while nothing flows (the roster-collapse
/// signature). Default [`crate::util::consts::STARVATION_THRESHOLD_SECS`]
/// (2× alive timeout); hidden flag `--starvation-threshold-secs`, its own
/// knob so the tests' short-evict profile doesn't arm it everywhere.
pub(crate) fn starvation_threshold_secs() -> u64 {
    current().starvation_threshold_secs
}

/// How long `beacon::ensure` eagerly waits for the freshly-bound
/// rendezvous to gossip-mesh with this process's own (already
/// subscribed) participant before returning. Closes the
/// rendezvous-readiness race: a joiner that dials the rendezvous finds
/// it already bridged into the swarm, not a bare socket. Bounded — on
/// timeout we fall through and the beacon's heal loop keeps the link
/// converging exactly as before (empty-room joinability preserved;
/// never blocks the event loop indefinitely). Generous enough to
/// cover a public endpoint's relay-home warmup, capped so a
/// pathological case can't stall startup.
pub(crate) const BEACON_MESH_WAIT_SECS: u64 = 8;

/// How long the event-driven failover burst keeps retrying
/// `beacon::ensure` after a beacon-loss `NeighborDown`. Must
/// comfortably exceed the departing beacon's graceful-shutdown grace
/// (it broadcasts `Left`, sleeps, then exits and releases the UDP
/// socket) so the survivor is still retrying when the port frees.
pub(crate) const RECLAIM_WINDOW_SECS: u64 = 6;

/// Cadence of the fast reclaim burst while the window (above) is open.
pub(crate) const RECLAIM_INTERVAL_MS: u64 = 400;

/// Max remembered `quiet` (silence-evicted but maybe-returning) participants.
/// `quiet` is drained only when a peer returns, so without a cap a churn / sybil
/// stream of one-shot nicknames would grow it without bound — the one unbounded
/// collection we own. Evicting a long-departed peer that never came back costs
/// only a missed `peer_return` surface — acceptable. Generously above any
/// realistic live roster.
pub(crate) const QUIET_CAP: usize = 1024;

/// Minimum gap between our own re-dial + `PeerInfo` re-flood of the *same*
/// peer learned via `PeerInfo` (`gossip::recv::handle_peer_info`). Caps the
/// membership amplifier so a flapping/unstable peer is re-linked at most once
/// per window instead of once per flap — the fix for the mesh-wide CPU
/// runaway. `10s`: exceeds the QUIC idle timeout (a truly-gone peer isn't
/// aggressively re-dialed) and is ≤ `HEAL_INTERVAL_SECS` (15s), so the healer
/// stays the backstop for legitimate re-bridge. iroh-gossip's own membership
/// still maintains links independently — this only throttles *our* piling-on.
pub(crate) const RELINK_COOLDOWN_SECS: u64 = 10;

/// How often an advertising `create` re-broadcasts its `🐝…` id into
/// the directory. Short enough that a fresh discoverer sees every live
/// swarm within one cycle (the join-horizon only surfaces ads stamped
/// after the discoverer joined), long enough that the directory stays
/// quiet — directory traffic is one tiny message per advertiser per
/// interval. Default [`crate::util::consts::ADVERTISE_INTERVAL_SECS`]; hidden
/// flag `--advertise-interval-secs` so the subprocess directory test re-ads
/// quickly.
pub(crate) fn advertise_interval_secs() -> u64 {
    current().advertise_interval_secs
}

/// How long a discoverer keeps showing a swarm after its last ad. A
/// publisher that exits stops re-broadcasting, so its listing ages out
/// within this window. ~3× `ADVERTISE_INTERVAL_SECS` so one or two lost
/// gossip rounds don't flicker a live swarm out of the list. Default
/// [`crate::util::consts::DIRECTORY_EXPIRY_SECS`]; hidden flag
/// `--directory-expiry-secs` so the subprocess directory test can shorten the
/// `swarm_lost` window.
pub(crate) fn directory_expiry_secs() -> u64 {
    current().directory_expiry_secs
}

/// Directories are public by default; the `--directory-private` flag flips
/// `directory_swarm` to the loopback ladder and relaxes the `--advertise`
/// requires-`--public` guard. **Test-only** (hidden flag): the live
/// advertise→discover path is otherwise unreachable in CI (no public relay) —
/// see `tests/directory.rs`.
pub(crate) fn directory_private_for_test() -> bool {
    current().directory_private
}

/// Per-rung timeout when selecting the public-mode bootstrap relay
/// ([`crate::lookup::select_bootstrap_rung`]) and when the beacon polls
/// its own rung's liveness. The selector walks the relay ladder and
/// homes on the first rung whose pinned endpoint reaches `online()`
/// within this budget; a rung that does not answer in time is treated
/// as unreachable and the next is tried.
///
/// Set to iroh's `NET_REPORT_TIMEOUT` (10s): `online()`'s own docs say
/// to use a timeout close to it so at least one net-report has been
/// attempted — a shorter budget can misjudge a healthy-but-slow relay
/// as down and trigger a spurious fall-through.
pub(crate) const RELAY_RUNG_PROBE_SECS: u64 = 10;

/// How often the beacon polls whether its current relay rung is still
/// connected (`timeout(RELAY_RUNG_PROBE_SECS, online())` on its own
/// endpoint). Off the event loop, inside the beacon co-host task.
pub(crate) const RELAY_LIVENESS_INTERVAL_SECS: u64 = 10;

/// Debounce: consecutive failed liveness polls before the beacon
/// concludes its rung is gone and re-walks the ladder. >1 so a single
/// transient blip (iroh auto-reconnects its home relay within a tick)
/// does not thrash the beacon between rungs.
pub(crate) const RELAY_LIVENESS_FAILS_TO_EVICT: u32 = 2;

/// Relay-less rediscovery backoff bounds. When the beacon holds **no**
/// rung (every ladder rung was unreachable), it keeps re-walking the
/// ladder to rediscover a recovered rung — but backs off between
/// rounds (`crate::lookup::next_relay_backoff`, doubling from MIN to
/// MAX) so an all-down ladder isn't hammered. The MIN is the first
/// inter-round wait; MAX caps it (still re-walking forever, just
/// sparsely). Distinct from `RELAY_LIVENESS_INTERVAL_SECS`, which is the
/// *homed* poll cadence (cheap, no backoff).
pub(crate) const RELAY_REPROBE_BACKOFF_MIN_SECS: u64 = 30;
pub(crate) const RELAY_REPROBE_BACKOFF_MAX_SECS: u64 = 300;

/// Timeout for the private-mode rendezvous identity probe. When a
/// ladder rung is `AddrInUse`, a member probes it to tell *our*
/// swarm's beacon (→ stay a participant) from an unrelated swarm
/// squatting the rung (→ try the next rung). The probe is a loopback
/// connect to a live listener, so it resolves in milliseconds; this
/// is only a guard against a pathological non-responding socket, kept
/// tight so a contended-rung walk can't stall the event loop.
pub(crate) const RENDEZVOUS_PROBE_SECS: u64 = 1;
