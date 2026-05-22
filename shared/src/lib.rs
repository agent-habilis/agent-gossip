//! Constants and helpers shared across the workspace (the `ahs` binary
//! crate and the xtask runner). Kept dependency-free so xtask stays light.

/// Runtime directory for sockets and other ephemeral files.
pub const TMP_DIR: &str = "/tmp/agent-habilis-swarm";

/// Per-member log dir. `AHS_LOG_DIR` overrides; default `{TMP_DIR}/logs`.
#[must_use]
pub fn log_dir() -> String {
    std::env::var("AHS_LOG_DIR").unwrap_or_else(|_| format!("{TMP_DIR}/logs"))
}

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

/// QUIC keep-alive interval in seconds. Keepalives on an otherwise idle
/// connection stop a quiet-but-live peer from being dropped; must stay
/// well below `QUIC_MAX_IDLE_SECS`.
pub const QUIC_KEEP_ALIVE_SECS: u64 = 5;

/// QUIC max idle timeout in seconds before a connection is considered
/// dead. Tightened from iroh's 15s (direct) / 30s (relay) path defaults
/// so a dead or slept peer is detected (`NeighborDown`) in ~10s, which
/// speeds up heal and the rendezvous-independent re-bridge.
pub const QUIC_MAX_IDLE_SECS: u64 = 10;
