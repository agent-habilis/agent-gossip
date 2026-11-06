//! The refusals of the shared state, through the real CLI: `agent-gossip state merge` exits with a
//! non-zero code, prints `{"ok":false,"error":...,"reason":...,"hint":...}` with the exact text of
//! the engine, and the way out works. The creator of a mesh has no first-sync wait, so these tests
//! write at once.

use std::process::Output;

use agent_gossip::Channel;
use agent_gossip_test_fixtures as common;
use common::{InProcNode, Node};
use serde_json::{Map, Value, json};

/// The JSON that a refused merge printed, after the check that it exited with a non-zero code.
fn refusal_of(output: &Output) -> Value {
    assert!(
        !output.status.success(),
        "a refused merge exits non-zero: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let response: Value =
        serde_json::from_slice(&output.stdout).expect("the CLI prints one JSON value on stdout");
    assert_eq!(response["ok"], false, "a refusal says ok:false: {response}");
    response
}

fn merge(node: &Node, mesh: &str, merge: &Value) -> Output {
    common::cli_channel_merge(Channel::State, mesh, &node.nickname, &merge.to_string())
}

/// `{"a":{"a":{ ... 1 ... }}}`, `levels` keys deep.
fn nested(levels: usize) -> Value {
    (0..levels).fold(json!(1), |inner, _| json!({ "a": inner }))
}

/// `{"live": {"0": 0, "1": 1, ...}}` with `count` leaves, numbered from `first`: short keys, so
/// that a merge holds as many leaves as one frame can carry.
fn leaves(first: usize, count: usize) -> Value {
    let mut under = Map::new();
    for leaf in first..first + count {
        under.insert(format!("{leaf:x}"), json!(leaf % 10));
    }
    json!({ "live": Value::Object(under) })
}

#[test]
fn a_value_nested_too_deep_is_refused_with_its_reason_and_the_way_out() {
    let (creator, mesh) = Node::create();
    assert!(creator.wait_ready(&mesh), "the creator never served");
    let accepted = merge(&creator, &mesh, &nested(12));
    assert!(accepted.status.success(), "12 levels are allowed");
    let response = refusal_of(&merge(&creator, &mesh, &nested(13)));
    assert_eq!(
        response["error"], "the value is nested 13 levels deep and the shared state allows 12",
        "the exact text: {response}"
    );
    assert_eq!(response["reason"], "too_deep", "the stable reason");
    assert!(
        response["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("fewer levels")),
        "an agent acts on the hint, so it names the next step: {response}"
    );
}

#[test]
fn a_value_too_large_for_a_frame_of_a_seed_is_refused() {
    let (creator, mesh) = Node::create();
    assert!(creator.wait_ready(&mesh), "the creator never served");
    let response = refusal_of(&merge(
        &creator,
        &mesh,
        &json!({ "note": "x".repeat(4_000) }),
    ));
    let error = response["error"]
        .as_str()
        .expect("error is a string in every refusal");
    assert!(
        error.starts_with("a value of the write is too large for the shared state: ")
            && error.ends_with(" bytes with its keys; a value and its keys must fit one frame"),
        "the exact text: {error}"
    );
    assert_eq!(response["reason"], "too_large", "the stable reason");
}

/// The state fills in one in-process node, so the test spawns no CLI process. The CLI path of a
/// refusal is proven by the two tests above, and the mapping of a full state by the unit test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "about 85 s of one CPU: 120 or more writes of one frame, and each write hydrates the whole document; run it with --ignored"]
async fn a_full_state_refuses_a_write_that_adds_and_a_null_merge_is_the_way_out() {
    let node = InProcNode::create("state-full").await;
    // A write is one frame of at most 3 840 bytes. The stamps grow with the clock, so the leaves
    // that fit one merge fall as the state fills: a merge that is too large for one frame is
    // sent again with a third fewer leaves.
    let mut step = 200;
    let mut next = 0;
    let mut refused: Option<String> = None;
    for _ in 0..600 {
        match node.try_state_merge(leaves(next, step)).await {
            Ok(()) => next += step,
            Err(error) if error.to_string().contains("message too large") => step = step * 2 / 3,
            Err(error) => {
                refused = Some(error.to_string());
                break;
            }
        }
    }
    let error = refused.expect("600 merges pass the line of 20 000 paths");
    assert!(
        error.starts_with("the shared state is full: ")
            && error.ends_with(
                "a write that adds a value is refused, also with the same value; delete keys with a null merge to free space, or wait for the next cut"
            ),
        "the exact text: {error}"
    );
    node.state_merge(json!({ "live": null })).await;
    node.leave().await;
}
