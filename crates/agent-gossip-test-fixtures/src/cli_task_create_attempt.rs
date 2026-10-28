use std::process::{Command, Output};

use crate::{RPC_ATTEMPT, test_cmd};

/// One `a2a call` attempt, budgeted at [`RPC_ATTEMPT`] rather than the CLI's
/// much larger default, so the retry in [`cli_task_create`] re-sends a dropped
/// frame promptly instead of blocking on a frame the overlay discarded.
pub(crate) fn cli_task_create_attempt(mesh: &str, nickname: &str, to: &str, text: &str) -> Output {
    let timeout = RPC_ATTEMPT.as_secs().to_string();
    let mut command: Command = test_cmd();
    command.args([
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
        "--timeout-secs",
        &timeout,
    ]);
    command.output().expect("a2a call command failed to spawn")
}
