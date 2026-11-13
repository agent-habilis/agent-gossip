//! Multipart message bodies, in-process over the real event loop + iroh mesh
//! (`common::InProcNode`). A body larger than the single-message wire cap
//! (`MAX_MESSAGE_SIZE`) is split by the sender into `shard`-tagged messages and
//! reassembled by the receiver; the split is invisible to agents on both ends.
//! These pin that round-trip for a plain `msg` and for a task content leg —
//! the body surfaces **once**, as the whole logical message, never as raw shards.

use agent_gossip::a2a::status_update_params;
use agent_gossip::{MAX_LOGICAL_BODY_BYTES, MAX_MESSAGE_SIZE, MessageBody, TaskId, TaskState};
use agent_gossip_test_fixtures as common;
use common::{BIG_BODY_TIMEOUT, InProcNode, MSG_TIMEOUT, chat_text};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multishard_message_reassembles_into_one() {
    let alice = InProcNode::create("mp-msg").await;
    let mut bob = InProcNode::join(&alice.mesh, "mp-msg-bob").await;

    // Several times the single-message cap, so the daemon must split it.
    let big = "abcd ".repeat(MAX_MESSAGE_SIZE); // ~19 KB
    let id = alice.broadcast(&big).await;

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
/// reassembles — the shard count is no longer capped — and the surfaced logical
/// body is the only trace.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multishard_message_past_the_old_cap_reassembles() {
    let mut alice = InProcNode::create("mp-big").await;
    let mut bob = InProcNode::join(&alice.mesh, "mp-big-bob").await;
    // Mesh first: an unmeshed multipart send buffers whole in the (64-slot)
    // pending-outbound queue, and ~55 shards would overflow it.
    common::warm_up(&alice, &mut [&mut bob]).await;
    common::warm_up(&bob, &mut [&mut alice]).await;

    // ~200 KB needs ~55 shards — well past the old 16-shard refusal.
    let big = "0123456789".repeat(20 * 1024);
    let id = alice.broadcast(&big).await;

    // `BIG_BODY_TIMEOUT`, not `MSG_TIMEOUT`: a dropped shard heals only through the
    // (deliberately lazy) `shard/repair` round — which cannot even begin inside a
    // 1-minute budget.
    assert!(
        bob.wait_body(&big, BIG_BODY_TIMEOUT).await,
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

#[cfg(feature = "adversarial")]
mod shard_repair {
    use std::time::{Duration, Instant};

    use agent_gossip::Nickname;
    use agent_gossip::api::A2aCallParams;
    use agent_gossip_test_fixtures as common;
    use common::{BIG_BODY_TIMEOUT, InProcNode};

    /// Send a `shard/repair` ask and check that the call times out: the ask has no reply.
    async fn ask_without_reply(requester: &InProcNode, author: &str, params: serde_json::Value) {
        let ask = A2aCallParams::new(
            Nickname::try_new(author).expect("the nickname of a node is valid"),
            "shard/repair".to_owned(),
            params,
            Duration::from_secs(1),
        );
        let response = requester
            .session
            .a2a_call(ask)
            .await
            .expect("a2a_call transport");
        assert!(
            response["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("timed out")),
            "a shard repair ask gets no reply, so the call times out: {response}"
        );
    }

    /// The count of shard frames that `node` re-sent, once it holds at least `at_least`, or the
    /// last count after 5 s.
    async fn shards_resent_by(node: &InProcNode, at_least: usize) -> usize {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let count = node
                .session
                .shards_resent()
                .await
                .expect("the session of the author stays up");
            if count >= at_least || Instant::now() >= deadline {
                return count;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// A big group's heal path is the `shard/repair` ask: the author caches its outbound shard
    /// frames and sends the ones that a receiver names as missing to that receiver only. The ask
    /// gets no reply. The proof is the count of frames that the author re-sent, which no other
    /// delivery of the body can fake: the gossip layer can still deliver the body by itself, so
    /// the arrival of the body would prove nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_shard_repair_ask_re_sends_the_cached_shards_to_the_requester() {
        let alice = InProcNode::create("mp-rep").await;
        let mut bob = InProcNode::join(&alice.mesh, "mp-rep-bob").await;
        let carol = InProcNode::join(&alice.mesh, "mp-rep-carol").await;
        common::warm_up(&alice, &mut [&mut bob]).await;

        // A >16-shard body: alice caches its frames for repair.
        let big = "0123456789".repeat(20 * 1024); // ~200 KB, ~55 shards
        let id = alice.broadcast(&big).await;
        assert!(
            bob.wait_body(&big, BIG_BODY_TIMEOUT).await,
            "big body arrived"
        );
        carol.await_peer_card(&alice.nickname).await;
        let before = alice
            .session
            .shards_resent()
            .await
            .expect("the session of the author stays up");
        assert_eq!(before, 0, "the premise: no repair was served yet");

        // The group id is the logical message id. The ask names every index that a group of this
        // size can hold, and the author skips the ones that it does not cache.
        let every_index: Vec<u32> = (0..64).collect();
        let params = serde_json::json!({ "group": id.as_str(), "missing": every_index });
        ask_without_reply(&carol, &alice.nickname, params).await;
        let resent = shards_resent_by(&alice, 50).await;
        assert!(
            resent >= 50,
            "the author re-sent {resent} cached shards to the requester, a group has more than 50"
        );

        // A group that the author never cached is dropped without a reply and re-sends nothing.
        let unknown =
            serde_json::json!({ "group": "00000000-0000-4000-8000-00000000dead", "missing": [0] });
        ask_without_reply(&carol, &alice.nickname, unknown).await;
        let after = alice
            .session
            .shards_resent()
            .await
            .expect("the session of the author stays up");
        assert_eq!(after, resent, "an unknown group re-sends no shard");
        // A round trip to the author still completes, so its event loop is not stuck.
        carol.await_peer_card(&alice.nickname).await;

        alice.leave().await;
        bob.leave().await;
        carol.leave().await;
    }
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
    alice.broadcast(&big).await;

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

    let body = MessageBody::try_new(&"x".repeat(MAX_LOGICAL_BODY_BYTES + 1)).expect("valid body");
    let error = alice.session.broadcast(body).await.expect_err(
        "a body over the cap would need more shards than a mesh accepts, so sending it is unsafe",
    );
    assert!(
        error.to_string().contains("too large"),
        "expected a too-large error, got: {error}"
    );

    alice.leave().await;
}

/// Large, shards like any content leg — the initiator reassembles it once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multishard_task_artifact_reassembles_into_one() {
    let mut alice = InProcNode::create("mp-ex").await;
    let mut bob = InProcNode::join(&alice.mesh, "mp-ex-bob").await;
    // Mesh first so the RPC + push are actually delivered.
    common::warm_up(&alice, &mut [&mut bob]).await;

    // Alice creates a task on bob (native SendMessage); bob mints the id.
    let response = alice.create_task("mp-ex-bob", "produce a big report").await;
    let task_id: TaskId = response["result"]["task"]["id"]
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
        .filter(|(message, _)| status_update_params::task_text(message) == big)
        .count();
    assert_eq!(
        matching, 1,
        "the artifact body must surface once, not once per shard"
    );

    alice.leave().await;
    bob.leave().await;
}
