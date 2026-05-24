//! Per-member log path resolution. Lives in the dependency-free shared
//! crate so the daemon (which writes the file) and `cargo task logs`
//! (which prints the dir) agree on one source of truth.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::consts::{LOG_FILE_MAX_BYTES, LOG_SUBPATH};
use crate::swarm_prefix;

/// Per-member log dir. `AHS_LOG_DIR` overrides; default is
/// [`LOG_SUBPATH`] under the OS temp dir (`std::env::temp_dir()` —
/// `/tmp/...` on Linux, a per-user dir on macOS). Sockets use
/// [`crate::consts::SOCKET_DIR`].
#[must_use]
pub fn log_dir() -> PathBuf {
    resolve_log_dir(std::env::var_os("AHS_LOG_DIR"), &std::env::temp_dir())
}

/// Pure resolver split out of [`log_dir`] so the policy is testable
/// without mutating process env: the override wins verbatim, else the
/// log subpath is joined onto the temp base.
fn resolve_log_dir(override_var: Option<OsString>, temp_base: &Path) -> PathBuf {
    override_var.map_or_else(|| temp_base.join(LOG_SUBPATH), PathBuf::from)
}

/// Per-member log file — `<swarm_prefix>-<nick>.log` (same stem as the
/// socket) under [`log_dir`].
#[must_use]
pub fn log_file_path(swarm_id: &str, nickname: &str) -> PathBuf {
    log_dir().join(format!("{}-{}.log", swarm_prefix(swarm_id), nickname))
}

/// Max bytes a log file grows before rotating to `<file>.1`.
/// `AHS_LOG_MAX_BYTES` overrides [`LOG_FILE_MAX_BYTES`]; `0` (default or
/// override) disables rotation.
#[must_use]
pub fn log_max_bytes() -> u64 {
    resolve_log_max_bytes(std::env::var_os("AHS_LOG_MAX_BYTES"))
}

/// Pure resolver split out of [`log_max_bytes`] for testing: a parseable
/// override wins; anything missing or malformed falls back to the default.
fn resolve_log_max_bytes(override_var: Option<OsString>) -> u64 {
    override_var
        .and_then(|raw| raw.to_str().and_then(|text| text.parse().ok()))
        .unwrap_or(LOG_FILE_MAX_BYTES)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{LOG_FILE_MAX_BYTES, resolve_log_dir, resolve_log_max_bytes};

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
        let dir = resolve_log_dir(Some("/custom/x".into()), Path::new("/tmp"));
        assert_eq!(dir, PathBuf::from("/custom/x"));
    }

    #[test]
    fn max_bytes_default_when_unset_or_invalid() {
        assert_eq!(resolve_log_max_bytes(None), LOG_FILE_MAX_BYTES);
        assert_eq!(
            resolve_log_max_bytes(Some("garbage".into())),
            LOG_FILE_MAX_BYTES
        );
    }

    #[test]
    fn max_bytes_override_parsed() {
        assert_eq!(resolve_log_max_bytes(Some("4096".into())), 4096);
        assert_eq!(
            resolve_log_max_bytes(Some("0".into())),
            0,
            "0 disables rotation"
        );
    }
}
