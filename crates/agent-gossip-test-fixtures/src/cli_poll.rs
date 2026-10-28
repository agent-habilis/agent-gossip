use crate::test_cmd;

/// Spawn `agent-gossip poll …`, assert success,
/// return trimmed stdout.
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
