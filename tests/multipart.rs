//! Multipart message bodies, in-process over the real event loop + iroh mesh
//! (`common::InProcNode`). A body larger than the single-message wire cap
//! (`MAX_MESSAGE_SIZE`) is split by the sender into `shard`-tagged messages and
//! reassembled by the receiver; the split is invisible to agents on both ends.
//! These pin that round-trip for a plain `msg` and for a task content leg —
//! the body surfaces **once**, as the whole logical message, never as raw shards.

mod common;

use agent_mesh::{MAX_LOGICAL_BODY_BYTES, MAX_MESSAGE_SIZE, MessageBody, TaskId, TaskState};
use common::{InProcNode, MSG_TIMEOUT, chat_text};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multishard_message_reassembles_into_one() {
    let alice = InProcNode::create("mp-msg").await;
    let mut bob = InProcNode::join(&alice.mesh, "mp-msg-bob").await;

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

/// A body past the old 16-shard ceiling (~60 KB) still splits and
/// reassembles — the shard count is no longer capped — and its shards skip
/// the message log (big groups must not evict the anti-entropy history), so
/// the surfaced logical body is the only trace.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multishard_message_past_the_old_cap_reassembles() {
    let mut alice = InProcNode::create("mp-big").await;
    let mut bob = InProcNode::join(&alice.mesh, "mp-big-bob").await;
    // Mesh first: an unmeshed multipart send buffers whole in the (64-slot)
    // pending-outbound queue, and ~55 shards would overflow it.
    alice.send("warmup").await;
    assert!(bob.wait_body("warmup", MSG_TIMEOUT).await, "mesh formed");
    bob.send("warmup-back").await;
    assert!(alice.wait_body("warmup-back", MSG_TIMEOUT).await, "reverse");

    // ~200 KB needs ~55 shards — well past the old 16-shard refusal.
    let big = "0123456789".repeat(20 * 1024);
    let id = alice.send(&big).await;

    assert!(
        bob.wait_body(&big, MSG_TIMEOUT).await,
        "the reassembled >16-shard body never arrived"
    );
    assert_eq!(bob.count_body(&big), 1, "the body surfaces exactly once");
    let received = bob
        .inbound()
        .into_iter()
        .find(|message| chat_text(message) == big)
        .expect("reassembled message present");
    assert_eq!(received.id, id, "named by the shard group on both ends");

    alice.leave().await;
    bob.leave().await;
}

/// A big (unlogged) group's heal path is the `shard/repair` RPC: the author
/// caches its outbound shard frames and re-delivers the ones a receiver names
/// as missing. Exercise the serve side end-to-end over the real RPC binding.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shard_repair_reserves_cached_big_group_frames() {
    let mut alice = InProcNode::create("mp-rep").await;
    let mut bob = InProcNode::join(&alice.mesh, "mp-rep-bob").await;
    alice.send("warmup").await;
    assert!(bob.wait_body("warmup", MSG_TIMEOUT).await, "mesh formed");
    bob.send("warmup-back").await;
    assert!(alice.wait_body("warmup-back", MSG_TIMEOUT).await, "reverse");

    // A >16-shard body: alice caches its frames for repair.
    let big = "0123456789".repeat(20 * 1024); // ~200 KB, ~55 shards
    let id = alice.send(&big).await;
    assert!(bob.wait_body(&big, MSG_TIMEOUT).await, "big body arrived");

    // Bob asks alice to re-send two shards of that group (the group id is the
    // logical message id). Bob already holds them — dedup will drop the
    // re-delivery — but the serve side must find and re-send them.
    let alice_nick = alice.nickname.clone();
    let resp = bob
        .a2a_call(
            &alice_nick,
            "shard/repair",
            serde_json::json!({ "group": id.as_str(), "missing": [0, 5] }),
        )
        .await;
    assert_eq!(
        resp["result"]["resent"], 2,
        "the author re-served the cached frames: {resp}"
    );

    // An unknown group is a clean zero, not an error.
    let miss = bob
        .a2a_call(
            &alice_nick,
            "shard/repair",
            serde_json::json!({
                "group": "00000000-0000-4000-8000-00000000dead",
                "missing": [0],
            }),
        )
        .await;
    assert_eq!(miss["result"]["resent"], 0, "evicted/unknown group: {miss}");

    alice.leave().await;
    bob.leave().await;
}

/// The same multishard round-trip on a **password-protected** mesh: the large
/// chat body is sealed with the mesh key, then split into shards; the receiver
/// reassembles the ciphertext envelope and decrypts it once. Exercises the
/// encrypt-before-shard (send) + reassemble-then-decrypt (receive) paths.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multishard_message_reassembles_on_a_passworded_mesh() {
    let alice = InProcNode::create_with_password("mp-pw", "hunter2").await;
    let mut bob = InProcNode::try_join_with_password(&alice.mesh, "mp-pw-bob", "hunter2")
        .await
        .expect("the right password joins");

    let big = "abcd ".repeat(MAX_MESSAGE_SIZE); // ~19 KB, forces sharding
    alice.send(&big).await;

    assert!(
        bob.wait_body(&big, MSG_TIMEOUT).await,
        "the reassembled, decrypted body never arrived on a passworded mesh"
    );
    assert_eq!(
        bob.count_body(&big),
        1,
        "the body must surface once, decrypted, not once per shard"
    );

    alice.leave().await;
    bob.leave().await;
}

/// The one remaining size limit is the local input ceiling
/// (`MAX_LOGICAL_BODY_BYTES`): a body past it is refused on send with a clear
/// error naming the blob channel, never silently dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn body_past_the_input_ceiling_is_refused() {
    let alice = InProcNode::create("mp-cap").await;

    let body = MessageBody::new("x".repeat(MAX_LOGICAL_BODY_BYTES + 1)).expect("valid body");
    let error = alice
        .session
        .send(body)
        .await
        .expect_err("a body past the input ceiling must be refused");
    assert!(
        error.to_string().contains("too large"),
        "expected a too-large error, got: {error}"
    );

    alice.leave().await;
}


/// large, shards like any content leg — the initiator reassembles it once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multishard_task_artifact_reassembles_into_one() {
    let mut alice = InProcNode::create("mp-ex").await;
    let mut bob = InProcNode::join(&alice.mesh, "mp-ex-bob").await;
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
        .filter(|(message, _)| agent_mesh::a2a::gossip::task_text(message) == big)
        .count();
    assert_eq!(
        matching, 1,
        "the artifact body must surface once, not once per shard"
    );

    alice.leave().await;
    bob.leave().await;
}
