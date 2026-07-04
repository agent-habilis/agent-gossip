//! Multipart message bodies, in-process over the real event loop + iroh mesh
//! (`common::InProcNode`). A body larger than the single-message wire cap
//! (`MAX_MESSAGE_SIZE`) is split by the sender into `part`-tagged messages and
//! reassembled by the receiver; the split is invisible to agents on both ends.
//! These pin that round-trip for a plain `msg` and for a task content leg —
//! the body surfaces **once**, as the whole logical message, never as raw parts.

mod common;

use agent_habilis_swarm::{MAX_MESSAGE_SIZE, TaskId, TaskPhase};
use common::{InProcNode, MSG_TIMEOUT};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multipart_message_reassembles_into_one() {
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
        "the body must surface once, not once per part"
    );
    let received = bob
        .inbound()
        .into_iter()
        .find(|message| message.body.as_str() == big)
        .expect("reassembled message present");
    assert_eq!(
        received.id, id,
        "sender and receiver name the reassembled body by the same id (its part group)"
    );

    alice.leave().await;
    bob.leave().await;
}

/// A body approaching the ~1 `MiB` logical ceiling round-trips and surfaces
/// once. This body is far larger than the old 16-part / ~60 `KiB` cap (it would
/// have been refused on send), so it pins the raised `MAX_MESSAGE_PARTS` +
/// message-log sizing: ~250 parts must all coexist in the receiver's log to
/// reassemble.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn near_max_logical_body_reassembles_into_one() {
    let alice = InProcNode::create("mp-big").await;
    let mut bob = InProcNode::join(&alice.swarm, "mp-big-bob").await;
    // Mesh first so the split body is actually delivered.
    alice.send("warmup").await;
    assert!(
        bob.wait_body("warmup", MSG_TIMEOUT).await,
        "mesh never formed"
    );

    // ~900 KB — over 14× the old 60 KiB ceiling, well under the new ~1 MiB one.
    let big = "packet01 ".repeat(100_000);
    let id = alice.send(&big).await;

    assert!(
        bob.wait_body(&big, MSG_TIMEOUT).await,
        "the near-max reassembled body never arrived"
    );
    assert_eq!(
        bob.count_body(&big),
        1,
        "the body must surface once, not once per part"
    );
    let received = bob
        .inbound()
        .into_iter()
        .find(|message| message.body.as_str() == big)
        .expect("reassembled message present");
    assert_eq!(
        received.id, id,
        "sender and receiver name it by the same id"
    );

    alice.leave().await;
    bob.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multipart_task_leg_reassembles_into_one() {
    let alice = InProcNode::create("mp-ex").await;
    let mut bob = InProcNode::join(&alice.swarm, "mp-ex-bob").await;
    // Mesh first so the leg is actually delivered.
    alice.send("warmup").await;
    assert!(
        bob.wait_body("warmup", MSG_TIMEOUT).await,
        "mesh never formed"
    );

    let task_id: TaskId = "550e8400-e29b-41d4-a716-446655440000"
        .parse()
        .expect("valid task id");
    let big = "step ".repeat(MAX_MESSAGE_SIZE); // ~19 KB
    alice
        .task("mp-ex-bob", &task_id, TaskPhase::Context, &big)
        .await
        .expect("the multipart leg is sent");

    assert!(
        bob.wait_task(TaskPhase::Context, MSG_TIMEOUT).await,
        "the reassembled task leg never arrived"
    );
    let matching = bob
        .tasks()
        .iter()
        .filter(|(message, _)| message.body.as_str() == big)
        .count();
    assert_eq!(
        matching, 1,
        "the task body must surface once, not once per part"
    );

    alice.leave().await;
    bob.leave().await;
}
