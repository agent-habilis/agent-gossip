use crate::test_cmd;

/// Worker-emit a task status via the CLI (`agent-gossip a2a status`). `text` and
/// `progress` go on the command line as given, with no default: a `working`
/// status needs both. Panics on failure.
///
/// # Panics
///
/// Panics if the CLI process fails to spawn or exits with an error.
#[expect(
    clippy::too_many_arguments,
    reason = "the helper mirrors the CLI flags one for one, so a test names each of them"
)]
pub fn cli_task_status(
    mesh: &str,
    nickname: &str,
    task_id: &str,
    state: &str,
    text: Option<&str>,
    progress: Option<&str>,
) {
    let mut command = test_cmd();
    command.args([
        "a2a",
        "status",
        "--gossip",
        mesh,
        "--nickname",
        nickname,
        "--task-id",
        task_id,
        "--state",
        state,
    ]);
    if let Some(text) = text {
        command.args(["--text", text]);
    }
    if let Some(progress) = progress {
        command.args(["--progress", progress]);
    }
    let out = command
        .output()
        .expect("a2a status command failed to spawn");
    assert!(
        out.status.success(),
        "a2a status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
