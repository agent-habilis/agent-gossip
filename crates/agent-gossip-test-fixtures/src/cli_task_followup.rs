use crate::{FollowupParams, test_cmd};

/// Send a **follow-up into an existing task** via the CLI.
///
/// A follow-up is an answer, an
/// approval, or a change request. The `--task-id` is what makes it a follow-up: A2A
/// reads an absent `taskId` as "no task yet", so the same call without it opens a
/// second, unrelated task on the worker. Returns the parsed JSON-RPC response,
/// whose `result.task.id` must be the id passed in.
///
/// # Panics
///
/// Panics if the CLI process fails or prints a response that is not JSON.
#[must_use]
pub fn cli_task_followup(params: &FollowupParams<'_>) -> serde_json::Value {
    let &FollowupParams {
        mesh,
        nickname,
        to,
        task_id,
        text,
    } = params;
    let out = test_cmd()
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
            "--task-id",
            task_id,
            "--text",
            text,
        ])
        .output()
        .expect("a2a call command failed to spawn");
    assert!(
        out.status.success(),
        "a2a follow-up failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(stdout.trim()).expect("a2a call prints a JSON-RPC response")
}
