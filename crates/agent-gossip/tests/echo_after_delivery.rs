use std::time::Duration;

use agent_gossip::a2a::status_update_params;
use agent_gossip::output_event::OutputEvent;
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
            OutputEvent::Message { message, is_self: true }
                if status_update_params::chat_text(message).as_deref() == Some(text)
        )
    })
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
async fn msg_to_a_live_peer_echoes_after_delivery() {
    let (mut alice, bob) = meshed_pair("echo-live", "echo-live-bob").await;

    alice.message("echo-live-bob", "arrives").await;

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
    for attempt in 0..45 {
        let body = format!("into the void {attempt}");
        let sent = common::cli_msg_raw(&mesh, &alice.nickname, &bob.nickname, &body);
        if sent.status.success() {
            std::thread::sleep(Duration::from_secs(2));
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

/// A send that reported an error must not reach the peer later through
/// anti-entropy: the user was told it failed, and may send it again.
#[test]
fn a_msg_whose_send_failed_never_arrives_later() {
    let _serial = common::serial_guard();
    let flags = [
        ("--alive-timeout-secs", "60"),
        ("--antientropy-interval-secs", "2"),
    ];
    let (alice, mesh) = Node::create_flags("echo-late", &flags);
    let bob = Node::join_flags(&mesh, "echo-late-bob", &flags);
    assert!(alice.wait_ready(&mesh), "alice never ready");
    assert!(bob.wait_ready(&mesh), "bob never ready");
    common::await_peer_card_cli(&mesh, &alice.nickname, &bob.nickname);

    bob.stop();
    let mut failed = None;
    for attempt in 0..30 {
        let body = format!("reported as failed {attempt}");
        if !common::cli_msg_raw(&mesh, &alice.nickname, &bob.nickname, &body)
            .status
            .success()
        {
            failed = Some(body);
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    bob.cont();
    let failed = failed.expect("no send to the stopped peer failed in 60 seconds");

    let marker = "sent after the resume";
    let relink_deadline = std::time::Instant::now() + Duration::from_mins(1);
    while !common::cli_msg_raw(&mesh, &alice.nickname, &bob.nickname, marker)
        .status
        .success()
    {
        assert!(
            std::time::Instant::now() < relink_deadline,
            "alice never reached bob again"
        );
        std::thread::sleep(Duration::from_secs(2));
    }
    // Several anti-entropy rounds after the link is back: a retained frame
    // would be re-sent to bob within them.
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let mut served = String::new();
    while std::time::Instant::now() < deadline {
        served.push_str(&common::cli_poll(&mesh, &bob.nickname, None));
        assert!(
            !served.contains(&failed),
            "a msg reported as failed arrived later: {served}"
        );
        std::thread::sleep(Duration::from_secs(1));
    }
    assert!(
        served.contains(marker),
        "the marker never arrived: {served}"
    );
}
