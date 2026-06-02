//! The single home for constants we may want to tune in the future:
//! runtime paths plus the network-wide wire constants. The few names the
//! external test/bench crates assert against are re-exported from the crate
//! root (see `lib.rs`); the rest stay crate-internal.

/// Unix socket runtime dir. Hardcoded `/tmp` base — short (avoids the
/// macOS `AF_UNIX` `sun_path` ~104-byte limit). Sibling agent-habilis
/// projects share the `/tmp/agent-habilis/` namespace.
pub const SOCKET_DIR: &str = "/tmp/agent-habilis/swarm/sockets";

/// Default per-member log dir, relative to the OS temp dir
/// (`std::env::temp_dir()`); the `--log-dir` flag overrides. Resolved by
/// [`crate::util::logs::log_dir`].
pub(crate) const LOG_SUBPATH: &str = "agent-habilis/swarm/logs";

/// Max bytes a per-member log file grows before rotating to `<file>.1`
/// (active + one backup ⇒ bounded at `2 ×` this). The `--log-max-bytes` flag
/// overrides; `0` disables rotation. Resolved by
/// [`crate::util::logs::log_max_bytes`].
pub(crate) const LOG_FILE_MAX_BYTES: u64 = 10 * 1024 * 1024; // 10 MiB

/// Per-identity message rate limit, enforced symmetrically on the send
/// and receive paths (same quota each direction). One limit covers all
/// messages — open broadcasts and directed replies alike, no per-kind
/// distinction. It is the swarm's published contract (agents must stay
/// within it), so it lives in the shared crate, not the binary's tuning.
///
/// Messages per minute per identity (60 = one per second sustained). The
/// token bucket's depth equals this value, so a sender may emit up to
/// this many back-to-back, then one per `60 / RATE_LIMIT_PER_MIN` seconds
/// thereafter. This is the default a swarm is created with; the effective
/// cap travels in the swarm id (`0` there means no rate limit).
pub const RATE_LIMIT_PER_MIN: u16 = 60;

/// Maximum size in bytes of a serialized swarm message. A network-wide
/// wire contract (must be uniform across members), so it lives here.
///
/// Kept below iroh-gossip's `DEFAULT_MAX_MESSAGE_SIZE` (4096) minus its
/// ~39-byte wire header: a message larger than gossip's payload budget
/// is silently dropped by the gossip layer (it never propagates and the
/// sender gets no error), so our cap must stay under it. A compile-time
/// assertion in the binary guards that relationship against the live
/// gossip constant; the value is hardcoded here rather than derived from
/// iroh-gossip's (so this module pulls in no dependency).
pub const MAX_MESSAGE_SIZE: usize = 3840;

/// Default number of recent messages each member retains in its in-memory
/// log (anti-entropy recovery source + poll/fetch history). A fixed value
/// (see `tuning::message_log_size`); edit + commit here to change it. A
/// bigger log lets a reconnecting peer recover a
/// longer gap. Not coupled to the IPC response cap — that is the separate,
/// fixed [`POLL_RESPONSE_MAX_MSGS`] (the log can exceed it; `poll` then
/// surfaces the most-recent window and anti-entropy carries the rest).
pub(crate) const DEFAULT_MESSAGE_LOG_SIZE: usize = 1000;

/// Max messages a single `poll` / `fetch_messages` returns — a **fixed**
/// IPC contract (the `ahs poll` client can't know the daemon's configured
/// log size, so the read cap can't depend on it). At the default log size
/// this equals the log, so `poll` returns everything; a larger configured
/// log just means `poll` surfaces the most-recent `POLL_RESPONSE_MAX_MSGS`.
pub(crate) const POLL_RESPONSE_MAX_MSGS: usize = 1000;

/// Max bytes for one stdin line. A raw message body larger than the wire
/// cap can never form a valid message, so the line read is capped there.
pub(crate) const MAX_STDIN_LINE_BYTES: usize = MAX_MESSAGE_SIZE;

/// Max bytes for one IPC command line: the same body in a JSON envelope
/// (swarm id, nickname, keys). 2× the wire cap is comfortable headroom.
pub(crate) const MAX_IPC_COMMAND_BYTES: usize = 2 * MAX_MESSAGE_SIZE;

/// Max bytes for one IPC response line: a poll returns at most
/// [`POLL_RESPONSE_MAX_MSGS`] messages, each ≤ the wire cap. Fixed (not tied
/// to the configurable log size) so the `poll` client has a stable read
/// bound.
pub(crate) const MAX_IPC_RESPONSE_BYTES: usize = POLL_RESPONSE_MAX_MSGS * MAX_MESSAGE_SIZE;

// ── Daemon tuning defaults ────────────────────────────────────────
//
// Behavioural knobs that used to be environment-overridable. They now live
// here as constants: an experiment is an *edit + commit* (under version
// control, with history), never an ephemeral shell var. Each is the default
// for the matching hidden CLI flag (`--alive-timeout-secs`, …) that the
// subprocess test suite passes to run with short timings; production reads the
// const. See `agent_habilis_swarm::util::tuning`.

/// How long a peer can go unheard before the sweeper evicts it. Must exceed
/// the alive-keepalive interval comfortably (3× absorbs one or two lost
/// rounds). Flag: `--alive-timeout-secs` (tests shorten it to seconds).
pub(crate) const ALIVE_TIMEOUT_SECS: u64 = 90;

/// How often the sweeper walks `last_seen` looking for expired peers.
/// Flag: `--sweep-interval-secs`.
pub(crate) const SWEEP_INTERVAL_SECS: u64 = 10;

/// Grace before an unmeshed joiner co-hosts the rendezvous anyway (empty
/// swarm ⇒ become the beacon for the next joiner). Flag:
/// `--beacon-cohost-grace-secs`.
pub(crate) const BEACON_COHOST_GRACE_SECS: u64 = 10;

/// How long an `ahs ping` round collects pongs before the daemon emits its
/// `ping_report`. Flag: `--ping-window-secs`.
pub(crate) const PING_WINDOW_SECS: u64 = 10;

/// A heal inter-tick gap above this (seconds) means the process was frozen
/// between ticks (App Nap / sleep) and must hard re-bootstrap. Safely above
/// the 15s heal interval so normal slack never trips it. Flag:
/// `--heal-stall-threshold-secs`.
pub(crate) const HEAL_STALL_THRESHOLD_SECS: u64 = 60;

/// How often an advertising `create` re-broadcasts its `ahs…` id into the
/// directory. Flag: `--advertise-interval-secs`.
pub(crate) const ADVERTISE_INTERVAL_SECS: u64 = 20;

/// How long a discoverer keeps showing a swarm after its last ad (~3×
/// `ADVERTISE_INTERVAL_SECS`). Flag: `--directory-expiry-secs`.
pub(crate) const DIRECTORY_EXPIRY_SECS: u64 = 60;

/// Max messages re-broadcast in response to one received digest, so a
/// far-behind peer can't trigger an unbounded backfill burst. Flag:
/// `--antientropy-max-resend`.
pub(crate) const ANTIENTROPY_MAX_RESEND: usize = 64;

/// Soft resident-memory threshold (`MiB`) above which the daemon emits a
/// one-shot `warn` on its slow prune tick — the in-process leak-visibility
/// signal. (Resident memory = the physical RAM the process holds; the resident
/// set size, RSS.) Warn-only; `0` disables. A pure const (no flag): an operator
/// tunes it by editing here.
pub(crate) const RESIDENT_MEMORY_WARN_MB: u64 = 1024;

/// HyParView **active view** capacity — the number of direct gossip neighbors
/// (open QUIC links) each member maintains per topic. A swarm at or below this
/// size forms a **full mesh** with nothing to shuffle, so it has **zero
/// membership churn** (and thus none of the per-connection-churn memory leak);
/// past it the overlay maintains a partial mesh and continuously
/// promotes/demotes peers (the churn). Raised from iroh-gossip's default of 5
/// to **32** so realistic agent swarms (≤ 33) stay churn-free. The ceiling is
/// performance, not correctness: each slot is a live connection + keepalive,
/// and a full mesh costs O(S²) broadcast amplification, so ~48–50 is the
/// practical upper bound on Pi-class hardware. Distinct from the
/// `DEFAULT_MAX_DIRECT_PEERS` (25) soft address-tracking cap. Flag (hidden):
/// `--active-view-capacity` — set it *small* to deliberately reproduce the
/// gossip-churn leak at any node count.
pub(crate) const GOSSIP_ACTIVE_VIEW_CAPACITY: usize = 32;

/// HyParView **passive view** capacity — the backup contact pool used for
/// healing/shuffle when active-view links drop. Kept ≥ 2× the active view
/// (iroh-gossip default 30). Flag (hidden): `--passive-view-capacity`.
pub(crate) const GOSSIP_PASSIVE_VIEW_CAPACITY: usize = 64;

// QUIC keep-alive / idle timeout are intentionally left at iroh's
// holepunch-tuned transport defaults (~1s keep-alive, 15s direct / 30s relay
// idle); see `lookup::build_endpoint`. A prior override (5s keep-alive, 10s
// idle) fought iroh's QUIC-multipath tuning and drove connection-churn — a
// per-connection memory leak. The sleep-wake reliability tests therefore freeze
// a peer past iroh's 15s direct-path idle to force a link death.
