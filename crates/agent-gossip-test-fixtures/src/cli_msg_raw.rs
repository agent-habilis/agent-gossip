use std::process::Output;

use crate::test_cmd;

/// Spawn `agent-gossip a2a msg …` — chat addressed to one peer. Returns the
/// raw [`Output`] so a caller can assert on failure (a msg legitimately fails
/// until the recipient's card has replicated).
#[must_use]
pub fn cli_msg_raw(mesh: &str, nickname: &str, to: &str, body: &str) -> Output {
    test_cmd()
        .args([
            "a2a",
            "msg",
            "--gossip",
            mesh,
            "--nickname",
            nickname,
            "--to",
            to,
            "--text",
            body,
        ])
        .output()
        .expect("a2a msg command failed to spawn")
}
