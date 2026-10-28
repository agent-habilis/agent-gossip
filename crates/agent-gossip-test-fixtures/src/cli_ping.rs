use crate::test_cmd;

/// Spawn `agent-gossip ping … `, assert success. Fire-and-forget — the RTT
/// report lands on the target daemon's own output stream, not here.
pub fn cli_ping(mesh: &str, nickname: &str) {
    let out = test_cmd()
        .args(["ping", "--gossip", mesh, "--nickname", nickname])
        .output()
        .expect("ping command failed to spawn");
    assert!(
        out.status.success(),
        "ping failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
