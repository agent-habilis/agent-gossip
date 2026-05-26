//! Behavioural knobs. Changing these affects timing, capacity, and
//! policy but never the on-the-wire format — the size cap belongs in
//! `protocol::message` (`MAX_MESSAGE_SIZE`).
//!
//! The poll/MCP buffer size (`DEFAULT_MESSAGE_LOG_SIZE`) lives in
//! `ahs_shared::consts` — it anchors the shared IPC response cap.

/// In-memory message-log capacity: how many recent messages each member
/// retains as the anti-entropy recovery source and poll/fetch history. A
/// bigger log lets a reconnecting peer recover a longer gap. Defaults to
/// [`ahs_shared::DEFAULT_MESSAGE_LOG_SIZE`] (1000); overridable per process
/// via `AHS_MESSAGE_LOG_SIZE`. Clamped to `>= 1`.
pub(crate) fn message_log_size() -> usize {
    env_usize("AHS_MESSAGE_LOG_SIZE", ahs_shared::DEFAULT_MESSAGE_LOG_SIZE).max(1)
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
/// message envelope (the `ahs…` id alone is ~80 chars) it stays under
/// `MAX_MESSAGE_SIZE` (3840) — guarded by the `digest_fits_gossip_cap`
/// test. Sized to a single gossip message, **not** the (larger,
/// configurable) log, which the rolling cursor sweeps across rounds.
pub(crate) const ANTIENTROPY_DIGEST_WINDOW_IDS: usize = 70;

/// Max messages re-broadcast in response to one received digest, so a
/// far-behind peer can't trigger an unbounded burst. This throttles
/// deep-backfill throughput (~`this × peers` messages per
/// `ANTIENTROPY_INTERVAL_SECS`); raised from 32 and overridable via
/// `AHS_ANTIENTROPY_MAX_RESEND` so large reconnect gaps catch up faster.
pub(crate) fn antientropy_max_resend() -> usize {
    env_usize("AHS_ANTIENTROPY_MAX_RESEND", 64).max(1)
}

/// Default max direct peer connections (gossip relays beyond this)
pub(crate) const DEFAULT_MAX_DIRECT_PEERS: usize = 25;

/// Capacity of the embed facade's inbound broadcast channel. Bounded
/// so a slow embedder never backpressures the gossip/membership loop;
/// under sustained lag the oldest buffered messages are dropped and
/// the consumer observes `RecvError::Lagged` (see `embed::SwarmSession`).
pub(crate) const EMBED_INBOUND_CAP: usize = 1024;

// The per-identity message rate (`RATE_LIMIT_PER_MIN`) is a published
// contract enforced on both send and receive, so it lives in the shared
// crate — see `ahs_shared::RATE_LIMIT_PER_MIN`. The prune TTL below is a
// private memory-management knob, not part of that contract.

/// Rate-limiter entries idle longer than this (seconds) are pruned, so
/// the per-author bucket map can't grow unbounded as nicknames churn.
pub(crate) const RATE_LIMITER_TTL_SECS: u64 = 600;

/// How often an idle daemon broadcasts a `Presence::Alive` keepalive.
/// Active talkers never emit one — any sent gossip message resets the
/// timer, so chatty swarms pay zero heartbeat cost.
pub(crate) const ALIVE_INTERVAL_SECS: u64 = 30;

/// How long a peer can go unheard before the sweeper evicts it.
/// Must exceed `ALIVE_INTERVAL_SECS` comfortably — 3x absorbs one or
/// two lost gossip rounds. Worst-case ghost window is
/// `ALIVE_TIMEOUT_SECS + SWEEP_INTERVAL_SECS`.
///
/// Overridable via the `ALIVE_TIMEOUT_SECS` env var so integration
/// tests that exercise eviction can run in seconds instead of minutes.
pub(crate) fn alive_timeout_secs() -> u64 {
    env_u64("ALIVE_TIMEOUT_SECS", 90)
}

/// How often the sweeper walks `last_seen` looking for expired peers.
/// Bounds the maximum statusline staleness from a peer's true
/// disappearance to its eviction. Overridable via `SWEEP_INTERVAL_SECS`.
pub(crate) fn sweep_interval_secs() -> u64 {
    env_u64("SWEEP_INTERVAL_SECS", 10)
}

/// Grace before an **unmeshed joiner** co-hosts the rendezvous anyway
/// (empty swarm ⇒ become the beacon for the next joiner). Rationale:
/// `EventLoopConfig::cohost`. Non-blocking — only consulted
/// on heal ticks, never delays `ready`; a joiner that meshes co-hosts
/// the moment it has a neighbor, well before this. Overridable via
/// `BEACON_COHOST_GRACE_SECS` for subprocess tests.
pub(crate) fn cohost_grace_secs() -> u64 {
    env_u64("BEACON_COHOST_GRACE_SECS", 10)
}

/// How long an `ahs ping` round collects pongs before the daemon
/// emits its `ping_report`. Long enough for a relayed round-trip
/// across the mesh; overridable via `PING_WINDOW_SECS` so tests don't
/// wait the full window.
pub(crate) fn ping_window_secs() -> u64 {
    env_u64("PING_WINDOW_SECS", 10)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(default)
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
/// re-bootstrap. Default 60s — safely above `HEAL_INTERVAL_SECS` (15s)
/// so normal slack never trips it. Env-overridable (like
/// `alive_timeout_secs`) only so subprocess tests drive it in seconds.
pub(crate) fn heal_stall_threshold_secs() -> u64 {
    env_u64("HEAL_STALL_THRESHOLD_SECS", 60)
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

/// Minimum gap between our own re-dial + `PeerInfo` re-flood of the *same*
/// peer learned via `PeerInfo` (`gossip::recv::handle_peer_info`). Caps the
/// membership amplifier so a flapping/unstable peer is re-linked at most once
/// per window instead of once per flap — the fix for the mesh-wide CPU
/// runaway. `10s`: exceeds the QUIC idle timeout (a truly-gone peer isn't
/// aggressively re-dialed) and is ≤ `HEAL_INTERVAL_SECS` (15s), so the healer
/// stays the backstop for legitimate re-bridge. iroh-gossip's own membership
/// still maintains links independently — this only throttles *our* piling-on.
pub(crate) const RELINK_COOLDOWN_SECS: u64 = 10;

/// How often an advertising `create` re-broadcasts its `ahs…` id into
/// the directory. Short enough that a fresh discoverer sees every live
/// swarm within one cycle (the join-horizon only surfaces ads stamped
/// after the discoverer joined), long enough that the directory stays
/// quiet — directory traffic is one tiny message per advertiser per
/// interval. Env-overridable (`ADVERTISE_INTERVAL_SECS`) so the
/// subprocess directory test re-ads quickly.
pub(crate) fn advertise_interval_secs() -> u64 {
    env_u64("ADVERTISE_INTERVAL_SECS", 20)
}

/// How long a discoverer keeps showing a swarm after its last ad. A
/// publisher that exits stops re-broadcasting, so its listing ages out
/// within this window. ~3× `ADVERTISE_INTERVAL_SECS` so one or two lost
/// gossip rounds don't flicker a live swarm out of the list.
/// Env-overridable (`DIRECTORY_EXPIRY_SECS`) so the subprocess directory
/// test can shorten the `swarm_lost` window.
pub(crate) fn directory_expiry_secs() -> u64 {
    env_u64("DIRECTORY_EXPIRY_SECS", 60)
}

/// Directories are public by default; setting `AHS_DIRECTORY_PRIVATE`
/// flips `directory_swarm` to the loopback ladder and relaxes the
/// `--advertise` requires-`--public` guard. **Test-only**: the live
/// advertise→discover path is otherwise unreachable in CI (no public
/// relay) — see `tests/directory.rs`.
pub(crate) fn directory_private_for_test() -> bool {
    std::env::var_os("AHS_DIRECTORY_PRIVATE").is_some()
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
