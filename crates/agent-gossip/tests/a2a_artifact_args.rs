//! Integration tests for the CLI-side argument checks of `a2a artifact`.

use agent_gossip_test_fixtures as common;
use common::test_cmd;

fn artifact(extra: &[&str]) -> std::process::Output {
    let mut command = test_cmd();
    command
        .args([
            "a2a",
            "artifact",
            "--gossip",
            "deadbeef",
            "--nickname",
            "calm-otter",
            "--task-id",
            "550e8400-e29b-41d4-a716-446655440000",
        ])
        .args(extra);
    command
        .output()
        .expect("the test binary is built before the tests run")
}

/// An artifact parks the task for review, so one with nothing to review is
/// refused before any IPC, with the message the daemon uses.
#[test]
fn an_artifact_without_a_text_or_a_file_fails_on_the_cli_side() {
    for extra in [&[][..], &["--text", ""][..], &["--text", "   "][..]] {
        let out = artifact(extra);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "{extra:?} leaves nothing to review, so the CLI must refuse it"
        );
        assert!(
            stderr.contains("an artifact needs a text or a file"),
            "{extra:?}: the error must name the rule, got: {stderr}"
        );
    }
}
