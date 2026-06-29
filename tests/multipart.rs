//! Multipart message bodies, in-process over the real event loop + iroh mesh
//! (`common::InProcNode`). A body larger than the single-message wire cap
//! (`MAX_MESSAGE_SIZE`) is split by the sender into `part`-tagged messages and
//! reassembled by the receiver; the split is invisible to agents on both ends.
//! These pin that round-trip for a plain `msg` and for an exchange content leg —
//! the body surfaces **once**, as the whole logical message, never as raw parts.

mod common;

use agent_habilis_swarm::{ExchangeId, ExchangeKind, ExchangePhase, MAX_MESSAGE_SIZE};
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multipart_exchange_leg_reassembles_into_one() {
    let alice = InProcNode::create("mp-ex").await;
    let mut bob = InProcNode::join(&alice.swarm, "mp-ex-bob").await;
    // Mesh first so the leg is actually delivered.
    alice.send("warmup").await;
    assert!(
        bob.wait_body("warmup", MSG_TIMEOUT).await,
        "mesh never formed"
    );

    let exchange_id: ExchangeId = "550e8400-e29b-41d4-a716-446655440000"
        .parse()
        .expect("valid exchange id");
    let big = "step ".repeat(MAX_MESSAGE_SIZE); // ~19 KB
    alice
        .exchange(
            "mp-ex-bob",
            &exchange_id,
            ExchangeKind::Task,
            ExchangePhase::Context,
            &big,
        )
        .await
        .expect("the multipart leg is sent");

    assert!(
        bob.wait_exchange(ExchangePhase::Context, MSG_TIMEOUT).await,
        "the reassembled exchange leg never arrived"
    );
    let matching = bob
        .exchanges()
        .iter()
        .filter(|(message, _)| message.body.as_str() == big)
        .count();
    assert_eq!(
        matching, 1,
        "the exchange body must surface once, not once per part"
    );

    alice.leave().await;
    bob.leave().await;
}
