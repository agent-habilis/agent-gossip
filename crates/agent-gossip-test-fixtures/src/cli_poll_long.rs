use std::time::{Duration, Instant};

use crate::test_cmd;

/// `agent-gossip poll --long` (long-poll; blocks until events arrive), returning the
/// JSON stdout and how long the call took — so a test can assert it blocked /
/// resolved promptly.
///
/// # Panics
///
/// Panics if the CLI process fails to spawn or exits with an error.
#[must_use]
pub fn cli_poll_long(mesh: &str, nickname: &str, after: Option<&str>) -> (String, Duration) {
    let mut args = vec!["poll", "--gossip", mesh, "--nickname", nickname, "--long"];
    if let Some(id) = after {
        args.extend(["--after", id]);
    }
    let started = Instant::now();
    let out = test_cmd()
        .args(&args)
        .output()
        .expect("poll --long command failed to spawn");
    let elapsed = started.elapsed();
    assert!(
        out.status.success(),
        "poll --long failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        elapsed,
    )
}
