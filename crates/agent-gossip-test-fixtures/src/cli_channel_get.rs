use agent_gossip::Channel;

use crate::{channel_subcommand, test_cmd};

/// Spawn `agent-gossip <channel> get … `, assert success, return trimmed stdout (the
/// raw `{ok, document}` JSON line). Drives the real CLI → IPC socket → daemon
/// read path the in-process harness bypasses.
pub fn cli_channel_get(channel: Channel, mesh: &str, nickname: &str) -> String {
    let out = test_cmd()
        .args([channel_subcommand(channel), "get"])
        .args(["--gossip", mesh, "--nickname", nickname])
        .output()
        .expect("channel get failed to spawn");
    assert!(
        out.status.success(),
        "{} get failed: {}",
        channel_subcommand(channel),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
