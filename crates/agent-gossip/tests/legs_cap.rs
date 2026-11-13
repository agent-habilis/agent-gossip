//! Integration tests for the caps on the own legs that wait for their send.

use agent_gossip::MAX_MESSAGE_SIZE;
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

/// Only a content leg that is split into shards waits in the list of unsent legs: a single frame
/// has its verdict at once. A cap of 0 therefore refuses a body that needs shards, and still sends
/// a body of one frame.
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

    // Several times the size of one frame, so the body must be split into shards.
    let split = "abcd ".repeat(MAX_MESSAGE_SIZE);
    let refused = common::cli_msg_raw(&mesh, &alice.nickname, &bob.nickname, &split);
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success(),
        "a cap of 0 leaves no room, so the command must exit with an error"
    );
    assert!(
        error.contains("own content legs still wait for their send"),
        "the caller must get the reason, got: {error}"
    );

    // The control: a body of one frame never waits in the list, so a cap of 0 does not refuse it.
    let single = common::cli_msg_raw(&mesh, &alice.nickname, &bob.nickname, "hello");
    assert!(
        single.status.success(),
        "a single frame takes no slot, so a cap of 0 sends it: {}",
        String::from_utf8_lossy(&single.stderr)
    );

    let accepted = common::cli_msg_raw(&mesh, &bob.nickname, &alice.nickname, &split);
    assert!(
        accepted.status.success(),
        "a node with the default cap sends its split content leg: {}",
        String::from_utf8_lossy(&accepted.stderr)
    );
}

/// A cap of 0 refuses every call that carries a file: the refusal comes from the send path of the
/// daemon, before the file is offloaded.
#[test]
fn a_call_with_a_file_past_the_blob_group_cap_is_refused_with_the_reason() {
    let _serial = common::serial_guard();
    let (alice, mesh) = Node::create_flags(
        "itest-blob-cap",
        &[("--call-blob-groups-per-task-cap", "0")],
    );
    let bob = Node::join(&mesh, "blob-cap-bob");
    assert!(
        alice.wait_ready(&mesh),
        "the refusal comes from a daemon that serves commands"
    );
    assert!(
        bob.wait_ready(&mesh),
        "the peer must be up, or the refusal could come from a missing peer"
    );
    common::await_peer_card_cli(&mesh, &alice.nickname, &bob.nickname);
    let file = std::env::temp_dir().join(format!("agent-gossip-blob-cap-{}", std::process::id()));
    std::fs::write(&file, b"a file for the call").expect("the call needs a file to refuse");

    let mut command = common::test_cmd();
    command
        .args([
            "a2a",
            "call",
            "--gossip",
            &mesh,
            "--nickname",
            &alice.nickname,
        ])
        .args(["--to", &bob.nickname, "--method", "SendMessage"])
        .args(["--text", "see file", "--file"])
        .arg(&file);
    let refused: std::process::Output = command
        .output()
        .expect("the test binary must start, or no refusal can be read");
    std::fs::remove_file(&file).ok();

    let error = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        !refused.status.success(),
        "a cap of 0 leaves no room, so the command must exit with an error"
    );
    assert!(
        error.contains("call files, so this call was not sent"),
        "the caller must get the reason, got: {error}"
    );
}
