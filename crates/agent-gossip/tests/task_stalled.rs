//! Integration tests for the stalled-task report to an initiator.

use std::time::{Duration, Instant};

use agent_gossip_test_fixtures as common;
use common::Node;

const FAST_STALL: [(&str, &str); 2] = [
    ("--task-keepalive-secs", "1"),
    ("--task-stall-check-secs", "2"),
];

/// A worker daemon whose agent polled once and then went silent, with no bell
/// armed, is reported to the initiator. The chain is the real one: the worker's
/// beat carries the reader keys, the initiator reads them and raises
/// `task_stalled`, and `poll` serves it as a visible event.
#[test]
fn a_worker_that_stops_polling_is_reported_stalled_to_the_initiator() {
    let _serial = common::serial_guard();
    let (alice, mesh) = Node::create_flags("itest-stall", &FAST_STALL);
    let bob = Node::join_flags(&mesh, "stall-bob", &FAST_STALL);
    assert!(
        alice.wait_ready(&mesh),
        "a stall report needs a ready initiator to receive it"
    );
    assert!(
        bob.wait_ready(&mesh),
        "a stall report needs a ready worker to stop polling"
    );

    let task_id = common::cli_task_create(&mesh, &alice.nickname, &bob.nickname, "build it");
    common::cli_task_status(&mesh, &bob.nickname, &task_id, "working");
    // One foreground poll records that bob's agent was reading; with no bell
    // after it, its silence is what the initiator measures.
    let _ = common::cli_poll(&mesh, &bob.nickname, None);

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut seen = String::new();
    while Instant::now() < deadline {
        seen.push_str(&common::cli_poll(&mesh, &alice.nickname, None));
        if seen.contains("\"event\":\"task_stalled\"") {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(
        seen.contains("\"event\":\"task_stalled\""),
        "alice never saw task_stalled for {}\nalice poll output:\n{seen}\nbob log:\n{}",
        bob.nickname,
        bob.log_tail(20),
    );
    assert!(
        seen.contains(&format!("`<{}>` has not polled", bob.nickname)),
        "the event must name the worker, got:\n{seen}"
    );
}

/// The other half: a worker whose agent keeps polling is never reported, so the
/// event above is a fact about silence and not a side effect of the beats.
#[test]
fn a_worker_that_keeps_polling_is_not_reported_stalled() {
    let _serial = common::serial_guard();
    let (alice, mesh) = Node::create_flags("itest-reading", &FAST_STALL);
    let bob = Node::join_flags(&mesh, "reading-bob", &FAST_STALL);
    assert!(
        alice.wait_ready(&mesh),
        "the no-stall check is void unless the initiator is ready to receive reports"
    );
    assert!(
        bob.wait_ready(&mesh),
        "the no-stall check is void unless the worker is ready to poll"
    );

    let task_id = common::cli_task_create(&mesh, &alice.nickname, &bob.nickname, "build it");
    common::cli_task_status(&mesh, &bob.nickname, &task_id, "working");

    // Four check intervals: the silent worker above is reported within one.
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut seen = String::new();
    while Instant::now() < deadline {
        let _ = common::cli_poll(&mesh, &bob.nickname, None);
        seen.push_str(&common::cli_poll(&mesh, &alice.nickname, None));
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(
        !seen.contains("\"event\":\"task_stalled\""),
        "a worker that polls must not be reported, got:\n{seen}"
    );
}
