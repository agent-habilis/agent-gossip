#![expect(
    missing_docs,
    reason = "an integration test crate has no public docs; the module path names it"
)]

use std::time::Instant;

use agent_gossip_test_fixtures::{InProcNode, MSG_TIMEOUT, Node, POLL};
use serde_json::Value;

const SHARED: &str = "held-dup";

/// The `nickname_held` events of `node`, as the documented JSON lines.
fn held_events(node: &mut InProcNode) -> Vec<Value> {
    node.json_events()
        .filter(|event| event["event"] == "nickname_held")
        .collect()
}

/// Wait until `node` has a `nickname_held` event.
async fn await_held(node: &mut InProcNode) -> Vec<Value> {
    let deadline = Instant::now() + MSG_TIMEOUT;
    loop {
        let events = held_events(node);
        if !events.is_empty() {
            return events;
        }
        assert!(
            Instant::now() < deadline,
            "{} never got a nickname_held event",
            node.nickname
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The rule is symmetric on purpose: a tie-break needs an input that the claimant chooses (an older
/// proof time, a lower key), so a forger would win it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn both_members_of_one_nickname_get_one_visible_nickname_held_event() {
    let mut first = InProcNode::create_with_nick("nickname-held", SHARED).await;
    let mut second = InProcNode::join(&first.mesh, SHARED).await;

    for node in [&mut first, &mut second] {
        let held = await_held(node).await;
        assert_eq!(held.len(), 1, "the event comes once: {held:?}");
        assert_eq!(held[0]["nickname"], SHARED, "it names the held nickname");
        assert_eq!(
            held[0]["is_visible"], true,
            "the event is visible, so a skill prints it"
        );
    }

    first.leave().await;
    second.leave().await;
}

/// The `nickname_held` lines of the output of a daemon. The log holds the stdout of the process, one JSON
/// object a line, and its stderr, which is no JSON.
fn held_lines(node: &Node) -> Vec<Value> {
    let parse = |line: &str| -> Option<Value> { serde_json::from_str(line).ok() };
    let events: Vec<Value> = node.log_contents().lines().filter_map(parse).collect();
    events
        .into_iter()
        .filter(|event| event["event"] == "nickname_held")
        .collect()
}

/// A daemon that runs on its own, not in the test process, prints the `nickname_held` event on its stdout, where a
/// monitor reads it, and not only in the ring of the poll. The line has the display text that a skill prints.
#[test]
fn both_members_of_one_nickname_print_a_nickname_held_line_on_stdout() {
    let folder =
        std::env::temp_dir().join(format!("agent-gossip-held-stdout-{}", std::process::id()));
    std::fs::create_dir_all(&folder).expect("the temp folder is writable");
    let (_host, mesh) = Node::create_named("held-stdout");
    // A state file for each daemon makes two members that claim one nickname, and not two runs of one member.
    let join_as_member = |name: &str| {
        let state = folder
            .join(format!("{name}.state.json"))
            .to_string_lossy()
            .to_string();
        Node::join_args(&mesh, SHARED, &["--state-file", &state], &[])
    };
    let first = join_as_member("first");
    let second = join_as_member("second");

    let deadline = Instant::now() + MSG_TIMEOUT;
    for node in [&first, &second] {
        let held = loop {
            let lines = held_lines(node);
            if !lines.is_empty() {
                break lines;
            }
            assert!(
                Instant::now() < deadline,
                "a daemon never printed a nickname_held line on stdout: {}",
                node.log_tail(20)
            );
            std::thread::sleep(POLL);
        };
        assert_eq!(
            held[0]["nickname"], SHARED,
            "the line names the held nickname"
        );
        assert_eq!(
            held[0]["is_visible"], true,
            "the line is visible, so a skill prints it"
        );
        let display = held[0]["display"].as_str().unwrap_or_default();
        assert!(
            display.contains(SHARED),
            "the line has no display text that names the nickname: {held:?}"
        );
    }

    drop((first, second));
    let _ = std::fs::remove_dir_all(&folder);
}
