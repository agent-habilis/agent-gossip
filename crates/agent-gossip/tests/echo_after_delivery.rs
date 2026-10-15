use std::time::Duration;

use agent_gossip::events::OutputEvent;
use agent_gossip_test_fixtures as common;
use common::{InProcNode, MSG_TIMEOUT, Node};

const SHORT_EVICT: [(&str, &str); 3] = [
    ("--alive-timeout-secs", "3"),
    ("--sweep-interval-secs", "1"),
    ("--heal-interval-secs", "3"),
];

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

#[test]
fn msg_whose_dial_fails_leaves_no_self_echo() {
    let _serial = common::serial_guard();
    let (alice, mesh) = Node::create_flags("echo-dead", &SHORT_EVICT);
    let bob = Node::join_flags(&mesh, "echo-dead-bob", &SHORT_EVICT);
    assert!(alice.wait_ready(&mesh), "alice never ready");
    assert!(bob.wait_ready(&mesh), "bob never ready");
    common::await_peer_card_cli(&mesh, &alice.nickname, &bob.nickname);

    bob.kill();
    // Whether the first sends find a closed connection depends on timing. Send
    // until one fails: the failure is the case under test, and a send that
    // still goes out over the old connection may rightly echo.
    for attempt in 0..9 {
        std::thread::sleep(Duration::from_secs(10));
        let body = format!("into the void {attempt}");
        let sent = common::cli_msg_raw(&mesh, &alice.nickname, &bob.nickname, &body);
        if sent.status.success() {
            continue;
        }
        let polled = common::cli_poll(&mesh, &alice.nickname, None);
        assert!(
            !polled.contains(&body),
            "a msg that was not delivered must not echo as sent: {polled}"
        );
        return;
    }
    panic!("no send to the killed peer failed in 90 seconds");
}
