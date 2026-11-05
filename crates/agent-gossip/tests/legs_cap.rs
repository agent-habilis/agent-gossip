//! Integration tests for the caps on the own legs that wait for their send.

use agent_gossip_test_fixtures as common;
use common::Node;

/// A cap of 0 makes the first leg refuse, so the test needs no peer that hangs. The refusal comes
/// from the real send path of the daemon: `a2a status` reaches it through the event loop. The
/// offer of a call is not an own leg (the initiator learns its task from the reply), so the leg
/// here is the status of a worker.
#[test]
fn a_task_leg_past_the_own_legs_cap_is_refused_with_the_reason() {
    let _serial = common::serial_guard();
    let (alice, mesh) = Node::create_named("itest-own-cap");
    let capped = Node::join_flags(&mesh, "own-cap-bob", &[("--own-legs-cap", "0")]);
    let open = Node::join(&mesh, "own-cap-carol");
    for node in [&alice, &capped, &open] {
        assert!(node.wait_ready(&mesh), "{} must be ready", node.nickname);
    }
    let task_capped = common::cli_task_create(&mesh, &alice.nickname, &capped.nickname, "build it");
    let task_open = common::cli_task_create(&mesh, &alice.nickname, &open.nickname, "build it");

    let status = |worker: &Node, task: &str| {
        common::test_cmd()
            .args([
                "a2a",
                "status",
                "--gossip",
                &mesh,
                "--nickname",
                &worker.nickname,
            ])
            .args(["--task-id", task, "--state", "working"])
            .output()
            .expect("the test binary must start, or no refusal can be read")
    };
    let refused = status(&capped, &task_capped);
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success(),
        "a cap of 0 leaves no room, so the command must exit with an error"
    );
    assert!(
        error.contains("own task legs still wait for their send"),
        "the caller must get the reason, got: {error}"
    );

    // The control: a worker with the default cap sends the same leg.
    let accepted = status(&open, &task_open);
    assert!(
        accepted.status.success(),
        "a node with the default cap sends its task leg: {}",
        String::from_utf8_lossy(&accepted.stderr)
    );
}

#[test]
fn a_content_leg_past_the_unsent_legs_cap_is_refused_with_the_reason() {
    let _serial = common::serial_guard();
    let (alice, mesh) = Node::create_flags("itest-unsent-cap", &[("--unsent-legs-cap", "0")]);
    let bob = Node::join(&mesh, "unsent-cap-bob");
    assert!(
        alice.wait_ready(&mesh),
        "the refusal comes from a daemon that serves commands"
    );
    assert!(
        bob.wait_ready(&mesh),
        "the peer must be up, or the refusal could come from a missing peer"
    );
    common::await_peer_card_cli(&mesh, &alice.nickname, &bob.nickname);
    common::await_peer_card_cli(&mesh, &bob.nickname, &alice.nickname);

    let refused = common::cli_msg_raw(&mesh, &alice.nickname, &bob.nickname, "hello");
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success(),
        "a cap of 0 leaves no room, so the command must exit with an error"
    );
    assert!(
        error.contains("own content legs still wait for their send"),
        "the caller must get the reason, got: {error}"
    );

    let accepted = common::cli_msg_raw(&mesh, &bob.nickname, &alice.nickname, "hello");
    assert!(
        accepted.status.success(),
        "a node with the default cap sends its content leg: {}",
        String::from_utf8_lossy(&accepted.stderr)
    );
}
