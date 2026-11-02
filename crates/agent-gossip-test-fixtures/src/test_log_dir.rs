use std::fs::{self};
use std::sync::OnceLock;

/// Per-test-process log dir so `cargo task test` never writes into
/// the operator's default `agent-gossip/logs`. Passed via the
/// global `--log-dir` flag.
pub fn test_log_dir() -> &'static str {
    static DIR: OnceLock<String> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir =
            std::env::temp_dir().join(format!("agent-gossip-test-logs-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        dir.to_string_lossy().into_owned()
    })
}
