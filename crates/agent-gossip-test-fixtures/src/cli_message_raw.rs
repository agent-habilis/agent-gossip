use std::process::Output;

use crate::test_cmd;

/// Broadcast a mesh chat message via the CLI.
///
/// It runs `agent-gossip a2a broadcast --text`. Returns the raw `Output` with no success assertion,
/// so a caller testing a failure path can inspect it.
///
/// # Panics
///
/// Panics if the CLI process fails to spawn.
#[must_use]
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
