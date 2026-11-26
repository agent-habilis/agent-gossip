use std::path::{Path, PathBuf};

/// The file beside `state_file` where a start that failed leaves its error. `ready` reads it, so
/// that a daemon that ended before it served is reported at once and not after the wait.
fn failure_file(state_file: &Path) -> PathBuf {
    let mut name = state_file.as_os_str().to_owned();
    name.push(".failed");
    PathBuf::from(name)
}

/// Remove the error of an earlier start of the same session, so that a retry is not judged by it.
pub(super) fn clear(state_file: &Path) {
    // A file that is not there is the normal case.
    let _ = std::fs::remove_file(failure_file(state_file));
}

/// Leave the error of a start that failed, for `ready`. A write that fails changes nothing for the
/// caller: `ready` then waits its limit, as it did before.
pub(super) fn record(state_file: &Path, error: &anyhow::Error) {
    let _ = std::fs::write(failure_file(state_file), format!("{error:#}"));
}

/// The error that a failed start left beside `state_file`, if one did.
pub(super) fn read(state_file: &Path) -> Option<String> {
    let text = std::fs::read_to_string(failure_file(state_file)).ok()?;
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::{clear, read, record};

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("start-failure-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the temp dir is writable");
        dir.join("session.json")
    }

    #[test]
    fn a_recorded_error_is_read_back_and_a_clear_removes_it() {
        let state_file = scratch("round-trip");
        assert_eq!(
            read(&state_file),
            None,
            "a session that never failed has no error"
        );
        record(&state_file, &anyhow::anyhow!("max connections is 4"));
        assert_eq!(
            read(&state_file).as_deref(),
            Some("max connections is 4"),
            "the error that a failed start recorded is the one that is read"
        );
        clear(&state_file);
        assert_eq!(read(&state_file), None, "a retry starts with no error");
    }
}
