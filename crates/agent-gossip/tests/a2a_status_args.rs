use agent_gossip_test_fixtures as common;
use common::test_cmd;

fn status(state: &str, extra: &[&str]) -> std::process::Output {
    test_cmd()
        .args([
            "a2a",
            "status",
            "--gossip",
            "deadbeef",
            "--nickname",
            "calm-otter",
            "--task-id",
            "550e8400-e29b-41d4-a716-446655440000",
            "--state",
            state,
        ])
        .args(extra)
        .output()
        .expect("a2a status spawns")
}

/// `--waiting` marks a working task as waiting on the user. On any other
/// state it has no meaning, so the CLI must refuse before any IPC and the
/// error must say why, not just that the flag is unknown.
#[test]
fn waiting_with_a_state_other_than_working_fails_on_the_cli_side() {
    for state in ["input-required", "completed", "failed", "canceled"] {
        let out = status(state, &["--waiting", "asks its user"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{state} with --waiting must fail");
        assert!(
            stderr.contains("--waiting requires --state working"),
            "{state}: the error must explain the flag, got: {stderr}"
        );
    }
}

/// With `--state working` the flag passes the CLI check. There is no daemon
/// here, so the call still fails, but on the missing session.
#[test]
fn waiting_with_working_passes_the_cli_check() {
    let out = status("working", &["--waiting", "asks its user"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("unexpected argument") && !stderr.contains("--waiting requires"),
        "the CLI must accept --waiting with --state working, got: {stderr}"
    );
}

/// A worker sends five states. The others are the peer's to set, so the
/// parser must name them as invalid.
#[test]
fn a_state_a_worker_never_sends_is_refused_by_the_parser() {
    for state in ["submitted", "rejected", "auth-required"] {
        let out = status(state, &[]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{state} must fail");
        assert!(
            stderr.contains("invalid task state"),
            "{state}: the parser must refuse it, got: {stderr}"
        );
    }
}
