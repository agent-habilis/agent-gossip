use crate::test_cmd;

/// Spawn `agent-gossip peers …`, assert success, return trimmed stdout (the
/// raw `{ok, peers, peer_count}` JSON line).
pub fn cli_peers(mesh: &str, nickname: &str) -> String {
    let out = test_cmd()
        .args(["peers", "--gossip", mesh, "--nickname", nickname])
        .output()
        .expect("peers command failed to spawn");
    assert!(
        out.status.success(),
        "peers failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
