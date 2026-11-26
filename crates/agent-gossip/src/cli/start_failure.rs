use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use habilis_network::protocol::UnixSecs;
use habilis_network::runtime::state_file;

use super::ready_state_file;

/// How many seconds before the start of a `ready` an error may be dated and still be the error of
/// the start that this `ready` waits for. The launcher and `ready` run side by side, so the daemon
/// can fail a moment before `ready` looks. An older error belongs to an earlier start of the same
/// session, which has the same state file.
const BEFORE_READY_SECS: u64 = 2;

/// The clock in whole seconds, as the state file writes it.
pub(super) fn now_secs() -> u64 {
    u64::try_from(UnixSecs::now().as_secs()).unwrap_or(0)
}

/// Whether a daemon is serving through `state_file` now. A start that fails must not write over it.
fn serving(state_file: &Path) -> bool {
    matches!(
        state_file::read_snapshot(state_file),
        Ok(Some(snapshot)) if snapshot.ready && ready_state_file::ready_is_fresh(snapshot.last_updated)
    )
}

/// Write `text` to `path` as one file that only the user reads, in one step: a reader sees the old
/// file or the new one, never half of it.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp-failed");
    let scratch = PathBuf::from(name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true).mode(0o600);
    let mut file = options.open(&scratch)?;
    file.write_all(text.as_bytes())?;
    std::fs::rename(&scratch, path)
}

/// Put the error of a start that failed into `state_file`, as a snapshot that is not ready, for
/// `ready`: the same file and the same freshness rule as a daemon that serves. A daemon that
/// serves through this file is not touched. A write that fails changes nothing for the caller:
/// `ready` then waits its limit, as it did before.
pub(super) fn record(state_file: &Path, error: &anyhow::Error) {
    if serving(state_file) {
        return;
    }
    let snapshot = serde_json::json!({
        "ready": false,
        "last_updated": now_secs(),
        "error": format!("{error:#}"),
    });
    let _ = write_private(state_file, &snapshot.to_string());
}

/// The error of a start that failed, if `state_file` holds one that this `ready` may act on: the
/// snapshot is not ready, it is fresh, and it is not older than the start of the `ready` (`ready_began`,
/// in seconds) by more than a moment.
pub(super) fn read(state_file: &Path, ready_began: u64) -> Option<String> {
    let contents = std::fs::read_to_string(state_file).ok()?;
    let snapshot: serde_json::Value = serde_json::from_str(&contents).ok()?;
    if snapshot["ready"].as_bool()? {
        return None;
    }
    let last_updated = snapshot["last_updated"].as_u64()?;
    if !ready_state_file::ready_is_fresh(last_updated)
        || last_updated + BEFORE_READY_SECS < ready_began
    {
        return None;
    }
    snapshot["error"].as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("start-failure-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the temp dir is writable");
        dir.join("session.json")
    }

    #[test]
    fn a_recorded_error_is_read_back_by_a_ready_that_began_after_it() {
        let state_file = scratch("round-trip");
        assert_eq!(
            super::read(&state_file, 0),
            None,
            "a session that never failed has no error"
        );
        super::record(&state_file, &anyhow::anyhow!("max connections is 4"));
        let now = super::now_secs();
        assert_eq!(
            super::read(&state_file, now).as_deref(),
            Some("max connections is 4"),
            "the error that a failed start recorded is the one that is read"
        );
    }

    #[test]
    fn an_error_older_than_the_ready_that_looks_is_not_read() {
        let state_file = scratch("stale");
        super::record(&state_file, &anyhow::anyhow!("an earlier start"));
        let later = super::now_secs() + 10;
        assert_eq!(
            super::read(&state_file, later),
            None,
            "the error of an earlier start of the session must not end the wait of a later one"
        );
    }

    #[test]
    fn a_failed_start_does_not_write_over_a_daemon_that_serves() {
        let state_file = scratch("serving");
        let now = super::now_secs();
        std::fs::write(
            &state_file,
            format!(r#"{{"ready": true, "last_updated": {now}, "nickname": "alive"}}"#),
        )
        .expect("the temp dir is writable");
        super::record(&state_file, &anyhow::anyhow!("another daemon already runs"));
        let kept = std::fs::read_to_string(&state_file).expect("the file is still there");
        assert!(
            kept.contains(r#""ready": true"#) && kept.contains("alive"),
            "the state of the daemon that serves is kept: {kept}"
        );
        assert_eq!(
            super::read(&state_file, now),
            None,
            "a serving file is no failure"
        );
    }

    #[test]
    fn the_snapshot_of_a_failed_start_is_private_to_the_user() {
        let state_file = scratch("mode");
        super::record(&state_file, &anyhow::anyhow!("a start that failed"));
        let mode = std::fs::metadata(&state_file)
            .expect("the file was written")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "the state file of the engine is private, and so is this"
        );
    }
}
