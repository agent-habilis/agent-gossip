use agent_gossip::output_event::OutputEvent;
use agent_gossip::{MAX_MESSAGE_SIZE, TaskState};
use agent_gossip_test_fixtures as common;
use common::{InProcNode, MSG_TIMEOUT};

fn receipts_from(node: &mut InProcNode, from: &str) -> Vec<(String, String)> {
    node.events()
        .iter()
        .filter_map(|event| {
            let OutputEvent::Receipt {
                from: author,
                ref_id,
                state,
                ..
            } = event
            else {
                return None;
            };
            (author.as_str() == from).then(|| (ref_id.clone(), state.clone()))
        })
        .collect()
}

async fn wait_receipt(node: &mut InProcNode, from: &str, ref_id: &str) -> bool {
    node.wait_for(MSG_TIMEOUT, |events| {
        events.iter().any(|event| {
            matches!(
                event,
                OutputEvent::Receipt { from: author, ref_id: seen, .. }
                    if author.as_str() == from && seen == ref_id
            )
        })
    })
    .await
}

/// The warmup proves that the mesh formed, and the peer card is what a
/// directed msg is sealed to: without both, the first send can fail.
async fn meshed_pair(name: &str, bob_nick: &str) -> (InProcNode, InProcNode) {
    let alice = InProcNode::create_with_nick(name, &format!("{name}-alice")).await;
    let mut bob = InProcNode::join(&alice.mesh, bob_nick).await;
    alice.broadcast("warmup").await;
    assert!(bob.wait_body("warmup", MSG_TIMEOUT).await, "mesh formed");
    alice.await_peer_card(bob_nick).await;
    (alice, bob)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn directed_msg_returns_a_delivered_receipt_whose_ref_is_the_msg_id() {
    let (mut alice, bob) = meshed_pair("rc-msg", "rc-msg-bob").await;

    let id = alice.message("rc-msg-bob", "are you there").await;

    assert!(
        wait_receipt(&mut alice, "rc-msg-bob", id.as_str()).await,
        "no receipt for the msg"
    );
    assert_eq!(
        receipts_from(&mut alice, "rc-msg-bob"),
        vec![(id.as_str().to_owned(), "delivered".to_owned())]
    );

    alice.leave().await;
    bob.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broadcast_produces_no_receipt() {
    let (mut alice, mut bob) = meshed_pair("rc-bcast", "rc-bcast-bob").await;

    alice.broadcast("to everyone").await;
    // The broadcast rides gossip and the control msg goes unicast, so the two
    // have no order. Wait for bob to take the broadcast first: a receipt for
    // it would then go out before the control's.
    assert!(
        bob.wait_body("to everyone", MSG_TIMEOUT).await,
        "broadcast lost"
    );
    let id = alice.message("rc-bcast-bob", "control").await;
    assert!(wait_receipt(&mut alice, "rc-bcast-bob", id.as_str()).await);

    assert_eq!(
        receipts_from(&mut alice, "rc-bcast-bob"),
        vec![(id.as_str().to_owned(), "delivered".to_owned())],
        "only the directed msg earns a receipt"
    );

    alice.leave().await;
    bob.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_leg_returns_a_delivered_receipt() {
    let (mut alice, mut bob) = meshed_pair("rc-leg", "rc-leg-bob").await;
    let response = alice.create_task("rc-leg-bob", "do the thing").await;
    let task_id: agent_gossip::TaskId = response["result"]["task"]["id"]
        .as_str()
        .expect("the worker returned a Task with an id")
        .parse()
        .expect("a valid task id");
    assert!(bob.wait_task_message(MSG_TIMEOUT).await, "brief surfaced");

    bob.task_status(&task_id, TaskState::Working, Some("on it"))
        .await;

    assert!(
        alice
            .wait_for(MSG_TIMEOUT, |events| {
                events.iter().any(|event| {
                    matches!(event, OutputEvent::Task { message, is_self: false }
                        if message.author.as_str() == "rc-leg-bob")
                })
            })
            .await,
        "the status leg never reached the initiator"
    );
    let leg_id = alice
        .tasks()
        .into_iter()
        .find(|(message, is_self)| !is_self && message.author.as_str() == "rc-leg-bob")
        .map(|(message, _)| message.id.as_str().to_owned())
        .expect("the leg is in the capture");

    assert!(
        wait_receipt(&mut bob, "rc-leg-alice", &leg_id).await,
        "no receipt for the status leg"
    );

    alice.leave().await;
    bob.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharded_msg_returns_exactly_one_delivered_receipt_for_the_msg_id() {
    let (mut alice, bob) = meshed_pair("rc-big", "rc-big-bob").await;

    let id = alice
        .message("rc-big-bob", &"abcd ".repeat(MAX_MESSAGE_SIZE))
        .await;

    assert!(
        wait_receipt(&mut alice, "rc-big-bob", id.as_str()).await,
        "no receipt for the sharded msg"
    );
    // The control msg follows the big one on the same path: once its receipt
    // is in, a second receipt for the big msg would have arrived as well.
    let control = alice.message("rc-big-bob", "control").await;
    assert!(wait_receipt(&mut alice, "rc-big-bob", control.as_str()).await);
    assert_eq!(
        receipts_from(&mut alice, "rc-big-bob"),
        vec![
            (id.as_str().to_owned(), "delivered".to_owned()),
            (control.as_str().to_owned(), "delivered".to_owned()),
        ]
    );

    alice.leave().await;
    bob.leave().await;
}

/// An agent that reads through the in-process API (MCP `fetch_messages`) is a
/// reader too: the fetch that serves a directed msg sends its `read` receipt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_in_process_fetch_sends_the_read_receipt() {
    let (mut alice, mut bob) = meshed_pair("rc-fetch", "rc-fetch-bob").await;
    bob.await_peer_card(&alice.nickname).await;

    let id = alice.message("rc-fetch-bob", "read me in process").await;
    assert!(
        bob.wait_body("read me in process", MSG_TIMEOUT).await,
        "bob never got the msg"
    );
    let batch = bob.session.fetch(None, false).await.expect("fetch");
    assert!(!batch.events.is_empty(), "the fetch served nothing to read");

    let read = alice
        .wait_for(MSG_TIMEOUT, |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    OutputEvent::Receipt { ref_id, state, .. }
                        if ref_id == id.as_str() && state == "read"
                )
            })
        })
        .await;
    assert!(read, "an in-process fetch sent no read receipt");

    alice.leave().await;
    bob.leave().await;
}

/// A task brief comes over the request/response binding, so it earns no
/// `delivered` receipt. The worker's read of it still sends `read`, with the
/// messageId of the brief as its ref.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reading_a_task_brief_sends_a_read_receipt() {
    let (mut alice, mut bob) = meshed_pair("rc-brief", "rc-brief-bob").await;
    bob.await_peer_card(&alice.nickname).await;
    alice.create_task("rc-brief-bob", "read this brief").await;
    assert!(bob.wait_task_message(MSG_TIMEOUT).await, "brief surfaced");

    bob.session.fetch(None, false).await.expect("fetch");

    let read = alice
        .wait_for(MSG_TIMEOUT, |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    OutputEvent::Receipt { from, state, .. }
                        if from.as_str() == "rc-brief-bob" && state == "read"
                )
            })
        })
        .await;
    assert!(read, "reading a task brief sent no read receipt");

    alice.leave().await;
    bob.leave().await;
}
