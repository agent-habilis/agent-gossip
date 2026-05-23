//! The single home for constants we may want to tune in the future:
//! runtime paths plus the network-wide wire constants. Kept in the
//! dependency-free `ahs-shared` crate so both the `ahs` binary and the
//! task runner read the same values.

/// Unix socket runtime dir. Hardcoded `/tmp` base — short (avoids the
/// macOS `AF_UNIX` `sun_path` ~104-byte limit) and Unix-only filesystem
/// (Windows uses named pipes that ignore the dir). Sibling
/// agent-habilis projects share the `/tmp/agent-habilis/` namespace.
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
/// thereafter.
pub const RATE_LIMIT_PER_MIN: u32 = 60;

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

/// Number of recent messages retained for the poll command / MCP
/// `fetch_messages` buffer — the contract for how much history a single
/// poll can return. Anchors [`MAX_IPC_RESPONSE_BYTES`].
pub const DEFAULT_MESSAGE_LOG_SIZE: usize = 200;

/// Max bytes for one stdin line. A raw message body larger than the wire
/// cap can never form a valid message, so the line read is capped there.
pub const MAX_STDIN_LINE_BYTES: usize = MAX_MESSAGE_SIZE;

/// Max bytes for one IPC command line: the same body in a JSON envelope
/// (swarm id, nickname, keys). 2× the wire cap is comfortable headroom.
pub const MAX_IPC_COMMAND_BYTES: usize = 2 * MAX_MESSAGE_SIZE;

/// Max bytes for one IPC response line: a poll returns at most the full
/// message buffer, each message ≤ the wire cap.
pub const MAX_IPC_RESPONSE_BYTES: usize = DEFAULT_MESSAGE_LOG_SIZE * MAX_MESSAGE_SIZE;

/// QUIC keep-alive interval in seconds. Keepalives on an otherwise idle
/// connection stop a quiet-but-live peer from being dropped; must stay
/// well below `QUIC_MAX_IDLE_SECS`.
pub const QUIC_KEEP_ALIVE_SECS: u64 = 5;

/// QUIC max idle timeout in seconds before a connection is considered
/// dead. Tightened from iroh's 15s (direct) / 30s (relay) path defaults
/// so a dead or slept peer is detected (`NeighborDown`) in ~10s, which
/// speeds up heal and the rendezvous-independent re-bridge.
pub const QUIC_MAX_IDLE_SECS: u64 = 10;
