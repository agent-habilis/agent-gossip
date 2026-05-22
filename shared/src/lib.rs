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
