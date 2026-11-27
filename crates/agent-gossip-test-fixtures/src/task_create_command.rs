use std::process::Command;

use crate::test_cmd;

/// The `a2a call` that creates a task, with no deadline flag: [`cli_task_create`](fn@crate::cli_task_create)
/// adds a short one for each attempt.
pub(crate) fn task_create_command(mesh: &str, nickname: &str, to: &str, text: &str) -> Command {
    let mut command = test_cmd();
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
    ]);
    command
}
