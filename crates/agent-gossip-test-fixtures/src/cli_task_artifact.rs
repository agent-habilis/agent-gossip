use crate::test_cmd;

/// Worker-emit a task artifact (the result) via the CLI. Panics on failure.
///
/// # Panics
///
/// Panics if the CLI process fails to spawn or exits with an error.
pub fn cli_task_artifact(mesh: &str, nickname: &str, task_id: &str, text: &str) {
    let out = test_cmd()
        .args([
            "a2a",
            "artifact",
            "--gossip",
            mesh,
            "--nickname",
            nickname,
            "--task-id",
            task_id,
            "--text",
            text,
        ])
        .output()
        .expect("a2a artifact command failed to spawn");
    assert!(
        out.status.success(),
        "a2a artifact failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
