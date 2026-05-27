//! Per-member log path resolution. Lives in the dependency-free shared
//! crate so the daemon (which writes the file) and `cargo task logs`
//! (which prints the dir) agree on one source of truth.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::consts::{LOG_FILE_MAX_BYTES, LOG_SUBPATH};
use crate::swarm_prefix;

/// Log-path config, installed **once** at startup from the `--log-dir` /
/// `--log-max-bytes` flags. The shared crate has no CLI, so the binary parses
/// the flags and calls [`configure`]; if it never does (e.g. `cargo task
/// logs`, or a path that doesn't log to files) the defaults apply. Replaces
/// the former `AHS_LOG_DIR` / `AHS_LOG_MAX_BYTES` env reads.
#[derive(Clone, Debug, Default)]
pub struct LogConfig {
    /// `--log-dir` override; `None` ⇒ [`LOG_SUBPATH`] under the OS temp dir.
    pub dir: Option<PathBuf>,
    /// `--log-max-bytes` override; `None` ⇒ [`LOG_FILE_MAX_BYTES`].
    pub max_bytes: Option<u64>,
}

static LOG_CONFIG: OnceLock<LogConfig> = OnceLock::new();

/// Install the log-path config, once. A second call is ignored.
pub fn configure(config: LogConfig) {
    let _ = LOG_CONFIG.set(config);
}

fn config() -> LogConfig {
    LOG_CONFIG.get().cloned().unwrap_or_default()
}

/// Per-member log dir. The `--log-dir` flag overrides; default is
/// [`LOG_SUBPATH`] under the OS temp dir (`std::env::temp_dir()` — `/tmp/...`
/// on Linux, a per-user dir on macOS). Sockets use
/// [`crate::consts::SOCKET_DIR`].
#[must_use]
pub fn log_dir() -> PathBuf {
    resolve_log_dir(config().dir, &std::env::temp_dir())
}

/// Pure resolver split out of [`log_dir`] so the policy is testable: the
/// override wins verbatim, else the log subpath is joined onto the temp base.
fn resolve_log_dir(override_dir: Option<PathBuf>, temp_base: &Path) -> PathBuf {
    override_dir.unwrap_or_else(|| temp_base.join(LOG_SUBPATH))
}

/// Per-member log file — `<swarm_prefix>-<nick>.log` (same stem as the
/// socket) under [`log_dir`].
#[must_use]
pub fn log_file_path(swarm_id: &str, nickname: &str) -> PathBuf {
    log_dir().join(format!("{}-{}.log", swarm_prefix(swarm_id), nickname))
}

/// Max bytes a log file grows before rotating to `<file>.1`. The
/// `--log-max-bytes` flag overrides [`LOG_FILE_MAX_BYTES`]; `0` disables
/// rotation.
#[must_use]
pub fn log_max_bytes() -> u64 {
    config().max_bytes.unwrap_or(LOG_FILE_MAX_BYTES)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::resolve_log_dir;

    #[test]
    fn default_is_log_subpath_under_temp_base() {
        let dir = resolve_log_dir(None, Path::new("/tmp"));
        assert!(
            dir.ends_with("agent-habilis/swarm/logs"),
            "default must nest under the log subpath: {}",
            dir.display()
        );
        assert!(dir.starts_with("/tmp"));
    }

    #[test]
    fn override_wins_verbatim() {
        let dir = resolve_log_dir(Some(PathBuf::from("/custom/x")), Path::new("/tmp"));
        assert_eq!(dir, PathBuf::from("/custom/x"));
    }
}
