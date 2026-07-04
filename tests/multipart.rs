//! Multipart message bodies, in-process over the real event loop + iroh mesh
//! (`common::InProcNode`). A body larger than the single-message wire cap
//! (`MAX_MESSAGE_SIZE`) is split by the sender into `shard`-tagged messages and
//! reassembled by the receiver; the split is invisible to agents on both ends.
//! These pin that round-trip for a plain `msg` and for a task content leg —
//! the body surfaces **once**, as the whole logical message, never as raw shards.

mod common;

use agent_gossip::{MAX_MESSAGE_SIZE, TaskId, TaskState};
use common::{InProcNode, MSG_TIMEOUT, chat_text};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multishard_message_reassembles_into_one() {
    let alice = InProcNode::create("mp-msg").await;
    let mut bob = InProcNode::join(&alice.swarm, "mp-msg-bob").await;

    // Several times the single-message cap, so the daemon must split it.
    let big = "abcd ".repeat(MAX_MESSAGE_SIZE); // ~19 KB
    let id = alice.send(&big).await;

    assert!(
        bob.wait_body(&big, MSG_TIMEOUT).await,
        "the reassembled body never arrived"
    );
    assert_eq!(
        bob.count_body(&big),
        1,
        "the body must surface once, not once per shard"
    );
    let received = bob
        .inbound()
        .into_iter()
        .find(|message| chat_text(message) == big)
        .expect("reassembled message present");
    assert_eq!(
        received.id, id,
        "sender and receiver name the reassembled body by the same id (its shard group)"
    );

    alice.leave().await;
    bob.leave().await;
}

/// A worker's result (`TaskArtifactUpdate`) is fire-and-forget push and, when
/// large, shards like any content leg — the initiator reassembles it once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multishard_task_artifact_reassembles_into_one() {
    let mut alice = InProcNode::create("mp-ex").await;
    let mut bob = InProcNode::join(&alice.swarm, "mp-ex-bob").await;
    // Mesh first so the RPC + push are actually delivered.
    alice.send("warmup").await;
    assert!(
        bob.wait_body("warmup", MSG_TIMEOUT).await,
        "mesh never formed"
    );

    // Alice creates a task on bob (native SendMessage); bob mints the id.
    let resp = alice.create_task("mp-ex-bob", "produce a big report").await;
    let task_id: TaskId = resp["result"]["task"]["id"]
        .as_str()
        .expect("the worker returned a Task id")
        .parse()
        .expect("valid task id");

    // Bob (the worker) emits a large artifact — it must shard and reassemble
    // once on Alice's side.
    let big = "step ".repeat(MAX_MESSAGE_SIZE); // ~19 KB
    bob.task_artifact(&task_id, &big).await;

    assert!(
        alice
            .wait_task_state(TaskState::InputRequired, MSG_TIMEOUT)
            .await,
        "the reassembled artifact never arrived (parks the task in input-required)"
    );
    let matching = alice
        .tasks()
        .iter()
        .filter(|(message, _)| agent_gossip::a2a::gossip::task_text(message) == big)
        .count();
    assert_eq!(
        matching, 1,
        "the artifact body must surface once, not once per shard"
    );

    alice.leave().await;
    bob.leave().await;
}
