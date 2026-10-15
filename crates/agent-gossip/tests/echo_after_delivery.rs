use agent_gossip::events::OutputEvent;
use agent_gossip_test_fixtures as common;
use common::{InProcNode, MSG_TIMEOUT};

fn has_self_echo(events: &[OutputEvent], text: &str) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            OutputEvent::Message { msg, is_self: true }
                if agent_gossip::a2a::gossip::chat_text(msg).as_deref() == Some(text)
        )
    })
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
async fn msg_to_a_live_peer_echoes_after_delivery() {
    let (mut alice, bob) = meshed_pair("echo-live", "echo-live-bob").await;

    alice.msg("echo-live-bob", "arrives").await;

    assert!(
        alice
            .wait_for(MSG_TIMEOUT, |events| has_self_echo(events, "arrives"))
            .await,
        "a delivered msg must echo"
    );

    alice.leave().await;
    bob.leave().await;
}
