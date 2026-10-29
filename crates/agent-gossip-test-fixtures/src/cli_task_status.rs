use crate::test_cmd;

/// Worker-emit a task status via the CLI (`agent-gossip a2a status`). Panics on failure.
///
/// # Panics
///
/// Panics if the CLI process fails to spawn or exits with an error.
pub fn cli_task_status(mesh: &str, nickname: &str, task_id: &str, state: &str) {
    let out = test_cmd()
        .args([
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
        ])
        .output()
        .expect("a2a status command failed to spawn");
    assert!(
        out.status.success(),
        "a2a status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
