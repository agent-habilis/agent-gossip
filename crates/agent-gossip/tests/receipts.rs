use agent_gossip::TaskState;
use agent_gossip::events::OutputEvent;
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

    let id = alice.msg("rc-msg-bob", "are you there").await;

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
    let (mut alice, bob) = meshed_pair("rc-bcast", "rc-bcast-bob").await;

    alice.broadcast("to everyone").await;
    // The msg after the broadcast is the control: once its receipt is in,
    // a receipt for the broadcast would have arrived as well.
    let id = alice.msg("rc-bcast-bob", "control").await;
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
                    matches!(event, OutputEvent::Task { msg, is_self: false }
                        if msg.author.as_str() == "rc-leg-bob")
                })
            })
            .await,
        "the status leg never reached the initiator"
    );
    let leg_id = alice
        .tasks()
        .into_iter()
        .find(|(msg, is_self)| !is_self && msg.author.as_str() == "rc-leg-bob")
        .map(|(msg, _)| msg.id.as_str().to_owned())
        .expect("the leg is in the capture");

    assert!(
        wait_receipt(&mut bob, "rc-leg-alice", &leg_id).await,
        "no receipt for the status leg"
    );

    alice.leave().await;
    bob.leave().await;
}
