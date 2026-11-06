//! The refusals of the shared state, through the real CLI: `agent-gossip state merge` exits with a
//! non-zero code, prints `{"ok":false,"error":...,"reason":...,"hint":...}` with the exact text of
//! the engine, and the way out works. The creator of a mesh has no first-sync wait, so these tests
//! write at once.

use std::process::Output;

use agent_gossip::Channel;
use agent_gossip_test_fixtures as common;
use common::Node;
use serde_json::{Map, Value, json};

/// The JSON that a refused merge printed, after the check that it exited with a non-zero code.
fn refusal_of(output: &Output) -> Value {
    assert!(
        !output.status.success(),
        "a refused merge exits non-zero: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let response: Value = serde_json::from_slice(&output.stdout).expect("the refusal is JSON");
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
        "the way out: {response}"
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
    let error = response["error"].as_str().expect("an error text");
    assert!(
        error.starts_with("a value of the write is too large for the shared state: ")
            && error.ends_with(" bytes with its keys; a value and its keys must fit one frame"),
        "the exact text: {error}"
    );
    assert_eq!(response["reason"], "too_large", "the stable reason");
}

#[test]
fn a_full_state_refuses_a_write_that_adds_and_a_null_merge_is_the_way_out() {
    let (creator, mesh) = Node::create();
    assert!(creator.wait_ready(&mesh), "the creator never served");
    // A write is one frame of at most 3 840 bytes. The stamps grow with the clock, so the leaves
    // that fit one merge fall as the state fills: a merge that is too large for one frame is
    // sent again with a third fewer leaves.
    let mut step = 200;
    let mut next = 0;
    let mut refused: Option<Output> = None;
    for _ in 0..600 {
        let output = merge(&creator, &mesh, &leaves(next, step));
        if output.status.success() {
            next += step;
        } else if String::from_utf8_lossy(&output.stdout).contains("message too large") {
            step = step * 2 / 3;
        } else {
            refused = Some(output);
            break;
        }
    }
    let output = refused.expect("the leaves fill the state");
    let response = refusal_of(&output);
    let error = response["error"].as_str().expect("an error text");
    assert!(
        error.starts_with("the shared state is full: ")
            && error.ends_with(
                "a write that adds a value is refused, also with the same value; delete keys with a null merge to free space, or wait for the next cut"
            ),
        "the exact text: {error}"
    );
    assert_eq!(response["reason"], "state_full", "the stable reason");
    let way_out = merge(&creator, &mesh, &json!({ "live": null }));
    assert!(
        way_out.status.success(),
        "a delete is never refused at the line: {}",
        String::from_utf8_lossy(&way_out.stdout)
    );
}
