use crate::test_cmd;

/// Spawn `agent-gossip poll …`, assert success,
/// return trimmed stdout.
///
/// # Panics
///
/// Panics if the CLI process fails to spawn or exits with an error.
#[must_use]
pub fn cli_poll(mesh: &str, nickname: &str, after: Option<&str>) -> String {
    let mut args = vec!["poll", "--gossip", mesh, "--nickname", nickname];
    if let Some(id) = after {
        args.extend(["--after", id]);
    }
    let out = test_cmd()
        .args(&args)
        .output()
        .expect("poll command failed to spawn");
    assert!(
        out.status.success(),
        "poll failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
