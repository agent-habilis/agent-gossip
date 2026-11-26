//! Integration tests for the CLI-side argument checks of `a2a status`.

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
        .expect("the test binary is built before the tests run")
}

/// `--waiting-on` and `--progress` only mean something while a task runs. On
/// any other state the CLI must refuse before any IPC and say why, not just
/// that the flag is unknown.
#[test]
fn waiting_on_and_progress_with_a_state_other_than_working_fail_on_the_cli_side() {
    for state in ["input-required", "completed", "failed", "canceled"] {
        for (flag, value, reason) in [
            ("--waiting-on", "user", "waiting-on requires state working"),
            ("--progress", "50", "progress requires state working"),
        ] {
            let out = status(state, &[flag, value]);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                !out.status.success(),
                "{flag} only makes sense while a task still runs, so {state} must be refused"
            );
            assert!(
                stderr.contains(reason),
                "{state}: the error must explain {flag}, got: {stderr}"
            );
        }
    }
}

/// A `working` status is a check-in, so it needs a text and a progress. There
/// is no daemon here, so a complete one still fails, but on the missing session.
#[test]
fn working_needs_a_text_and_a_progress_on_the_cli_side() {
    for extra in [
        &[][..],
        &["--text", "on it"][..],
        &["--progress", "50"][..],
        &["--text", "  ", "--progress", "50"][..],
    ] {
        let out = status("working", extra);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "{extra:?} is an incomplete check-in, so the CLI must refuse it"
        );
        assert!(
            stderr.contains("working requires a text and a progress"),
            "{extra:?}: the error must name the rule, got: {stderr}"
        );
    }
    let out = status(
        "working",
        &[
            "--text",
            "on it",
            "--progress",
            "50",
            "--waiting-on",
            "user",
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("unexpected argument") && !stderr.contains("requires"),
        "a complete check-in must pass the CLI check, got: {stderr}"
    );
}

/// The progress is a whole number from 0 to 100; the parser rejects the rest.
#[test]
fn a_progress_out_of_range_is_a_parse_error() {
    for value in ["101", "50%", "half"] {
        let out = status("working", &["--text", "on it", "--progress", value]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "--progress {value} is not a whole percentage, so the parser must fail"
        );
        assert!(
            stderr.contains("progress"),
            "{value}: the parser must name the flag, got: {stderr}"
        );
    }
}

/// A worker sends five states. The others are the peer's to set, so the
/// parser must name them as invalid.
#[test]
fn a_state_a_worker_never_sends_is_refused_by_the_parser() {
    for state in ["submitted", "rejected", "auth-required"] {
        let out = status(state, &[]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "{state} is a state only the peer sets, so the parser must fail"
        );
        assert!(
            stderr.contains("invalid task state"),
            "{state}: the parser must refuse it, got: {stderr}"
        );
    }
}
