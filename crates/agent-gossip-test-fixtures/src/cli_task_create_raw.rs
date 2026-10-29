use std::process::Output;

use crate::test_cmd;

/// Create a task on `to` via the CLI (`agent-gossip a2a call --to <peer> --method
/// SendMessage --text`) and return the raw `Output` (no success assertion —
/// callers that test the unknown-peer failure path inspect it).
///
/// Deliberately **unguarded**: it is the helper the unknown-peer and
/// unicast-retry tests use, and waiting on a card that will never arrive (or
/// that the test means to race) is exactly what they must not do. Callers
/// targeting a known peer want [`cli_task_create`].
///
/// # Panics
///
/// Panics if the CLI process fails to spawn.
#[must_use]
pub fn cli_task_create_raw(mesh: &str, nickname: &str, to: &str, text: &str) -> Output {
    test_cmd()
        .args([
            "a2a",
            "call",
            "--gossip",
            mesh,
            "--nickname",
            nickname,
            "--to",
            to,
            "--method",
            "SendMessage",
            "--text",
            text,
        ])
        .output()
        .expect("a2a call command failed to spawn")
}
