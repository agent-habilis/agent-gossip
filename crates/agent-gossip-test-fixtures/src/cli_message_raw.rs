use std::process::Output;

use crate::test_cmd;

/// Broadcast a mesh chat message via the CLI — `agent-gossip a2a call --method
/// SendMessage --text` with no `--to` (A2A is point-to-point, so a mesh-wide
/// message declares itself). Returns the raw `Output` with no success assertion,
/// so a caller testing a failure path can inspect it.
pub fn cli_message_raw(mesh: &str, nickname: &str, body: &str) -> Output {
    test_cmd()
        .args([
            "a2a",
            "broadcast",
            "--gossip",
            mesh,
            "--nickname",
            nickname,
            "--text",
            body,
        ])
        .output()
        .expect("a2a broadcast command failed to spawn")
}
