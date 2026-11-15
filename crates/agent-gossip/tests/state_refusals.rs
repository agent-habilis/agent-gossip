//! The refusals of the shared state, through the real CLI: `agent-gossip state merge` exits with a
//! non-zero code, prints `{"ok":false,"error":...,"reason":...,"hint":...}` with the exact text of
//! the engine, and the way out works.

use std::process::Output;

use agent_gossip::Channel;
use agent_gossip::api::SessionError;
use agent_gossip_test_fixtures as common;
use common::{InProcNode, Node};
use habilis_network::runtime::{SendError, SliceError};
use serde_json::{Value, json};

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

/// Whether the session refused a merge because the value of the author would pass the value cap.
fn is_value_too_large(error: &SessionError) -> bool {
    let SessionError::Loop(cause) = error else {
        return false;
    };
    matches!(
        cause.downcast_ref::<SendError>(),
        Some(SendError::State(SliceError::ValueTooLarge { .. }))
    )
}

/// The type of the error is the contract, not its text: a caller that gets `ValueTooLarge` stores a blob.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_merge_past_the_value_cap_is_a_value_too_large_error_by_type() {
    let node = InProcNode::create("oversize").await;

    let error = node
        .try_state_merge(json!({ "note": "x".repeat(1_048_576 + 1_024) }))
        .await
        .expect_err("a value past 1 MiB is refused");

    assert!(is_value_too_large(&error), "the error: {error:?}");
    node.leave().await;
}

/// A value that does not fit one frame of 3 840 bytes is no longer refused: the engine sends it as chunks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_merge_larger_than_one_frame_is_taken_as_chunks() {
    let node = InProcNode::create("chunked").await;

    node.try_state_merge(json!({ "note": "x".repeat(5_000) }))
        .await
        .expect("5 000 bytes travel as chunks and are not refused");

    node.leave().await;
}

/// The help of `state merge` and `meta merge` must not promise what the CLI refuses: a top-level merge that
/// is not an object is refused (the document root is a map), so the help says that the top level is an
/// object, and not that a non-object value replaces the document.
#[test]
fn the_help_of_a_merge_says_that_the_top_level_is_an_object() {
    for channel in ["state", "meta"] {
        let output = common::test_cmd()
            .args([channel, "merge", "--help"])
            .output()
            .expect("the help of a merge prints");
        let text = String::from_utf8_lossy(&output.stdout);
        let words: Vec<&str> = text.split_whitespace().collect();
        let help = words.join(" ");
        assert!(
            !help.contains("replaces the document"),
            "the help of {channel} merge still says that a non-object value replaces the document: {help}"
        );
        assert!(
            help.contains("top-level merge must be an object"),
            "the help of {channel} merge must say that the top-level merge must be an object: {help}"
        );
    }
}

/// A value of 65 KB with 32769 structure units: far under the byte cap, and over the unit cap. The CLI carries
/// the merge in one argument, which Linux limits to 128 KiB, so a value past 1 MiB cannot go through it.
#[test]
fn a_value_with_too_many_units_is_refused_with_its_reason_and_the_way_out() {
    let (creator, mesh) = Node::create();
    assert!(creator.wait_ready(&mesh), "the creator never served");
    let response = refusal_of(&merge(&creator, &mesh, &json!({ "k": vec![1; 32_768] })));
    let error = response["error"]
        .as_str()
        .expect("error is a string in every refusal");
    assert!(
        error.starts_with("the value has ") && error.contains(" structure units, past the cap of "),
        "the exact text: {error}"
    );
    assert_eq!(response["reason"], "value_too_complex", "the stable reason");
    assert!(
        response["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("blob")),
        "an agent acts on the hint, so it names the next step: {response}"
    );
}
