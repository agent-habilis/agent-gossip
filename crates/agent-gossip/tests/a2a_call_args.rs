use agent_gossip_test_fixtures as common;
use common::test_cmd;

fn call_without_task_id(method: &str) -> std::process::Output {
    test_cmd()
        .args([
            "a2a",
            "call",
            "--gossip",
            "deadbeef",
            "--nickname",
            "calm-otter",
            "--to",
            "wire-thistle",
            "--method",
            method,
        ])
        .output()
        .expect("a2a call spawns")
}

/// Without `--task-id` these methods used to send `{}` to the peer. The CLI
/// must refuse before any IPC, so the error names the flag and not a missing
/// session.
#[test]
fn a_task_method_without_task_id_fails_on_the_cli_side() {
    for method in ["GetTask", "CancelTask", "SubscribeToTask"] {
        let out = call_without_task_id(method);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "{method} without --task-id must fail"
        );
        assert!(
            stderr.contains("--task-id"),
            "{method}: the error must name --task-id, got: {stderr}"
        );
    }
}
