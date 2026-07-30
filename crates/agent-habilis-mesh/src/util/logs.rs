//! Per-member log path resolution and the body-redaction policy. The daemon
//! (which writes the file) and the rest of the binary agree on one source of
//! truth here.

use std::path::PathBuf;
use std::sync::OnceLock;

use crate::util::consts::LOG_FILE_MAX_BYTES;
use crate::util::mesh_prefix;

/// Log config, installed **once** at startup from the `--log-dir` /
/// `--log-max-bytes` / `--log-raw` flags. The consumer parses the flags and
/// calls [`configure`]; if it never does (a path that doesn't log to files)
/// the defaults apply.
#[derive(Clone, Debug, Default)]
pub struct LogConfig {
    /// The consumer's runtime base (see
    /// [`runtime_base`](crate::util::runtime_base)) — the default log root when
    /// `dir` is unset. `None` only on a path that never opens a log file.
    pub base: Option<PathBuf>,
    /// `--log-dir` override; `None` ⇒ the per-mesh folder under `base`.
    pub dir: Option<PathBuf>,
    /// `--log-max-bytes` override; `None` ⇒ [`LOG_FILE_MAX_BYTES`].
    pub max_bytes: Option<u64>,
    /// `--log-raw`: log raw message bodies. Default `false` — bodies are
    /// redacted to length + content-hash so log files are safe to share. Opt
    /// in only for a dev's own local debugging.
    pub raw: bool,
}

static LOG_CONFIG: OnceLock<LogConfig> = OnceLock::new();

/// Install the log-path config, once. A second call is ignored.
pub fn configure(config: LogConfig) {
    let _ = LOG_CONFIG.set(config);
}

fn config() -> LogConfig {
    LOG_CONFIG.get().cloned().unwrap_or_default()
}

/// Log base dir. The `--log-dir` flag overrides; default is the configured
/// runtime base (the per-user, `0700` base sockets + state files also use). The
/// per-mesh subfolder is added by [`log_file_path`].
///
/// Falls back to the temp dir only when a consumer configured neither — a path
/// that never opens a log file, so the value is unobservable.
#[must_use]
pub fn log_dir() -> PathBuf {
    let config = config();
    resolve_log_dir(config.dir, config.base)
}

/// The consumer's runtime base, for the fail-closed parent check the log sink
/// runs before it opens a file (see
/// [`ensure_parent_private`](crate::util::ensure_parent_private)). `None` when
/// no consumer configured one.
pub(crate) fn configured_base() -> Option<PathBuf> {
    config().base
}

/// Resolver split out of [`log_dir`] so the policy is testable: the override
/// wins verbatim, else the consumer's runtime base.
fn resolve_log_dir(override_dir: Option<PathBuf>, base: Option<PathBuf>) -> PathBuf {
    override_dir.or(base).unwrap_or_else(std::env::temp_dir)
}

/// Per-member log file — `<mesh_prefix>/<nick>.tracing.log`, inside the
/// mesh's runtime folder beside its `<nick>.ipc.sock` / `<nick>.state.json`.
/// The sink's `open()` creates the parent dir, so the nesting needs no
/// pre-creation here.
#[must_use]
pub(crate) fn log_file_path(mesh_id: &str, nickname: &str) -> PathBuf {
    log_dir()
        .join(mesh_prefix(mesh_id))
        .join(format!("{nickname}.tracing.log"))
}

/// Max bytes a log file grows before rotating to `<file>.1`. The
/// `--log-max-bytes` flag overrides [`LOG_FILE_MAX_BYTES`]; `0` disables
/// rotation.
#[must_use]
pub(crate) fn log_max_bytes() -> u64 {
    config().max_bytes.unwrap_or(LOG_FILE_MAX_BYTES)
}

/// Whether message bodies are logged raw. Default `false`: bodies are
/// redacted to length + a short content-hash prefix so log files stay safe
/// to share with developers. `--log-raw` opts a local debugging run back
/// into raw bodies. See [`LogConfig::raw`].
#[must_use]
pub(crate) fn log_raw() -> bool {
    config().raw
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::resolve_log_dir;

    #[test]
    fn default_is_the_configured_runtime_base() {
        let base = crate::util::runtime_base("demo");
        assert_eq!(resolve_log_dir(None, Some(base.clone())), base);
    }

    #[test]
    fn override_wins_verbatim() {
        let dir = resolve_log_dir(
            Some(PathBuf::from("/custom/x")),
            Some(PathBuf::from("/base")),
        );
        assert_eq!(dir, PathBuf::from("/custom/x"));
    }
}
