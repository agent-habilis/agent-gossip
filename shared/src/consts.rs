//! The single home for constants we may want to tune in the future:
//! runtime paths plus the network-wide wire constants. Kept in the
//! dependency-free `ahs-shared` crate so both the `ahs` binary and the
//! task runner read the same values.

/// Unix socket runtime dir. Hardcoded `/tmp` base — short (avoids the
/// macOS `AF_UNIX` `sun_path` ~104-byte limit). Sibling agent-habilis
/// projects share the `/tmp/agent-habilis/` namespace.
pub const SOCKET_DIR: &str = "/tmp/agent-habilis/swarm/sockets";

/// Default per-member log dir, relative to the OS temp dir
/// (`std::env::temp_dir()`); `AHS_LOG_DIR` overrides. Resolved by
/// [`crate::logs::log_dir`].
pub const LOG_SUBPATH: &str = "agent-habilis/swarm/logs";

/// Max bytes a per-member log file grows before rotating to `<file>.1`
/// (active + one backup ⇒ bounded at `2 ×` this). `AHS_LOG_MAX_BYTES`
/// overrides; `0` disables rotation. Resolved by
/// [`crate::logs::log_max_bytes`].
pub const LOG_FILE_MAX_BYTES: u64 = 10 * 1024 * 1024; // 10 MiB

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
/// gossip constant — `ahs-shared` stays dependency-free, so the value is
/// hardcoded here rather than derived.
pub const MAX_MESSAGE_SIZE: usize = 3840;

/// Default number of recent messages each member retains in its in-memory
/// log (anti-entropy recovery source + poll/fetch history). Overridable per
/// process via `AHS_MESSAGE_LOG_SIZE` (see `tuning::message_log_size`); this
/// is just the default. A bigger log lets a reconnecting peer recover a
/// longer gap. Not coupled to the IPC response cap — that is the separate,
/// fixed [`POLL_RESPONSE_MAX_MSGS`] (the log can exceed it; `poll` then
/// surfaces the most-recent window and anti-entropy carries the rest).
pub const DEFAULT_MESSAGE_LOG_SIZE: usize = 1000;

/// Max messages a single `poll` / `fetch_messages` returns — a **fixed**
/// IPC contract (the `ahs poll` client can't know the daemon's configured
/// log size, so the read cap can't depend on it). At the default log size
/// this equals the log, so `poll` returns everything; a larger configured
/// log just means `poll` surfaces the most-recent `POLL_RESPONSE_MAX_MSGS`.
pub const POLL_RESPONSE_MAX_MSGS: usize = 1000;

/// Max bytes for one stdin line. A raw message body larger than the wire
/// cap can never form a valid message, so the line read is capped there.
pub const MAX_STDIN_LINE_BYTES: usize = MAX_MESSAGE_SIZE;

/// Max bytes for one IPC command line: the same body in a JSON envelope
/// (swarm id, nickname, keys). 2× the wire cap is comfortable headroom.
pub const MAX_IPC_COMMAND_BYTES: usize = 2 * MAX_MESSAGE_SIZE;

/// Max bytes for one IPC response line: a poll returns at most
/// [`POLL_RESPONSE_MAX_MSGS`] messages, each ≤ the wire cap. Fixed (not tied
/// to the configurable log size) so the `poll` client has a stable read
/// bound.
pub const MAX_IPC_RESPONSE_BYTES: usize = POLL_RESPONSE_MAX_MSGS * MAX_MESSAGE_SIZE;

// QUIC keep-alive / idle timeout are intentionally left at iroh's
// holepunch-tuned transport defaults (~1s keep-alive, 15s direct / 30s relay
// idle); see `lookup::build_endpoint`. A prior override (5s keep-alive, 10s
// idle) fought iroh's QUIC-multipath tuning and drove connection-churn — a
// per-connection memory leak. The sleep-wake reliability tests therefore freeze
// a peer past iroh's 15s direct-path idle to force a link death.
