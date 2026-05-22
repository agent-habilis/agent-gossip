//! Constants and helpers shared across the workspace (the `ahs` binary
//! crate and the xtask runner). Kept dependency-free so xtask stays light.

/// Runtime directory for sockets and other ephemeral files.
pub const TMP_DIR: &str = "/tmp/agent-habilis-swarm";

/// Per-member log dir. `AHS_LOG_DIR` overrides; default `{TMP_DIR}/logs`.
#[must_use]
pub fn log_dir() -> String {
    std::env::var("AHS_LOG_DIR").unwrap_or_else(|_| format!("{TMP_DIR}/logs"))
}
