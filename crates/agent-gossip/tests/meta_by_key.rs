#![expect(
    missing_docs,
    reason = "an integration test crate has no public docs; the module path names it"
)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Instant;

use agent_gossip::Channel;
use agent_gossip_test_fixtures as common;
use common::{CONNECT_TIMEOUT, InProcNode, MSG_TIMEOUT, Node, POLL, RECOVERY_TIMEOUT};
use serde_json::Value;

const SHARED: &str = "dup";

/// The `(slot, card)` pairs of `doc` whose card names `SHARED`, in slot order.
fn shared_cards(doc: &Value) -> Vec<(String, Value)> {
    let Some(peers) = doc.get("peers").and_then(Value::as_object) else {
        return Vec::new();
    };
    let names_shared = |entry: &Value| {
        let name: Option<&str> = entry.pointer("/card/name").and_then(Value::as_str);
        name == Some(SHARED)
    };
    let named = peers.iter().filter(|(_, entry)| names_shared(entry));
    named
        .map(|(slot, entry)| (slot.clone(), entry["card"].clone()))
        .collect()
}

fn is_key_hex(slot: &str) -> bool {
    let is_hex_digit = |byte: u8| matches!(byte, b'0'..=b'9' | b'a'..=b'f');
    slot.len() == 64 && slot.bytes().all(is_hex_digit)
}

/// Wait until `node` holds `count` complete cards that name `SHARED`.
async fn await_shared_cards(node: &InProcNode, count: usize) -> Vec<(String, Value)> {
    let deadline = Instant::now() + MSG_TIMEOUT;
    loop {
        let cards = shared_cards(&node.meta_get().await);
        let has_endpoint = |(_, card): &(String, Value)| card.get("endpoint").is_some();
        if cards.len() == count && cards.iter().all(has_endpoint) {
            return cards;
        }
        assert!(
            Instant::now() < deadline,
            "{} never held {count} cards of '{SHARED}': {cards:?}",
            node.nickname
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Two live members use one nickname. A third member holds two entries, one for each key, and
/// each card carries the endpoint of its own member. Two members that share one nickname are two
/// entries in the meta document, one slot for each author key: the card, the seal key and the route
/// of a send follow the key, never the name.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_keys_of_one_nickname_publish_two_cards() {
    let alice = InProcNode::create_with_nick("mbk-cards", SHARED).await;
    let beta = InProcNode::join(&alice.mesh, SHARED).await;
    let observer = InProcNode::join(&alice.mesh, "mbk-observer").await;

    let cards = await_shared_cards(&observer, 2).await;

    for (slot, _) in &cards {
        assert!(
            is_key_hex(slot),
            "the slot '{slot}' is not an author key in hex"
        );
    }
    assert_ne!(cards[0].0, cards[1].0, "the two keys share one slot");
    let endpoint = |card: &Value| card.pointer("/endpoint/id").cloned();
    assert_ne!(
        endpoint(&cards[0].1),
        endpoint(&cards[1].1),
        "the two cards carry one endpoint: the card of a key was read for the other key"
    );

    alice.leave().await;
    beta.leave().await;
    observer.leave().await;
}

/// A send by a nickname that two live keys use is refused with a clear error. When one of the
/// keys leaves, the same send reaches the other key, which opens it: the body is sealed to the
/// key that the frame is addressed to.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_send_by_a_shared_nickname_is_refused_and_then_reaches_the_remaining_key() {
    let mut alice = InProcNode::create_with_nick("mbk-send", SHARED).await;
    let beta = InProcNode::join(&alice.mesh, SHARED).await;
    let observer = InProcNode::join(&alice.mesh, "mbk-sender").await;
    let _ = await_shared_cards(&observer, 2).await;

    let refused = observer
        .try_msg(SHARED, "to-one-of-two")
        .await
        .expect_err("a nickname of two live keys names no member");
    let text = refused.to_string();
    assert!(
        text.contains(SHARED) && (text.contains("two or more") || text.contains("ambiguous")),
        "the refusal does not say why: {text}"
    );

    beta.leave().await;
    let deadline = Instant::now() + RECOVERY_TIMEOUT;
    loop {
        let sent = observer.try_msg(SHARED, "to-the-remaining-key").await;
        if sent.is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the send never reached the remaining key: {sent:?}"
        );
        tokio::time::sleep(POLL).await;
    }
    assert!(
        alice.wait_body("to-the-remaining-key", MSG_TIMEOUT).await,
        "the remaining key never opened the message"
    );

    alice.leave().await;
    observer.leave().await;
}

/// One GET against the local A2A binding; returns `(status, body)`.
fn get(port: u16, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to the a2a port");
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .expect("the server stays up until the client has sent its request");
    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("read the response");
    let code: &str = raw
        .split_whitespace()
        .nth(1)
        .expect("a well-formed HTTP response starts with a status line");
    let status: u16 = code.parse().expect("the status code is a number");
    let (_, body) = raw.split_once("\r\n\r\n").unwrap_or_default();
    (status, body.to_owned())
}

/// The port of the local A2A binding, once the daemon is ready.
fn wait_port(state_file: &std::path::Path) -> u16 {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    while Instant::now() < deadline {
        if let Ok(raw) = std::fs::read_to_string(state_file)
            && let Ok(parsed) = serde_json::from_str::<Value>(&raw)
            && parsed["ready"] == true
            && let Some(port) = parsed["a2a_port"].as_u64()
        {
            return u16::try_from(port).expect("a port fits u16");
        }
        std::thread::sleep(POLL);
    }
    panic!("the daemon never reported a bound a2a port");
}

/// The peer-card route takes a nickname or the key in hex. A nickname of two live keys gives a
/// clear error, and the key in hex gives the card of that key.
#[test]
fn the_peer_card_route_refuses_a_shared_nickname_and_serves_a_key() {
    let state_file = std::env::temp_dir().join(format!(
        "agent-gossip-meta-by-key-{}.state.json",
        std::process::id()
    ));
    let state_path = state_file.to_string_lossy().to_string();
    let (host, mesh) = Node::create_args(
        "mbk-route",
        &["--a2a-serve", "--state-file", &state_path],
        &[],
    );
    let first = Node::join(&mesh, SHARED);
    let second = Node::join(&mesh, SHARED);
    assert!(host.wait_ready(&mesh), "the host socket never appeared");
    let port = wait_port(&state_file);

    let deadline = Instant::now() + MSG_TIMEOUT;
    let slots = loop {
        let raw = common::cli_channel_get(Channel::Meta, &mesh, &host.nickname);
        let doc: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        let cards = shared_cards(&doc["document"]);
        if cards.len() == 2 {
            break cards;
        }
        assert!(
            Instant::now() < deadline,
            "the host never held two cards of '{SHARED}': {raw}"
        );
        std::thread::sleep(POLL);
    };

    let (status, body) = get(
        port,
        &format!("/peers/{SHARED}/.well-known/agent-card.json"),
    );
    assert_eq!(status, 404, "a nickname of two live keys is served: {body}");
    assert!(
        body.contains("two or more"),
        "the error does not say that the nickname names two members: {body}"
    );

    let (key_status, key_body) = get(
        port,
        &format!("/peers/{}/.well-known/agent-card.json", slots[0].0),
    );
    assert_eq!(
        key_status, 200,
        "the card of a key in hex is not served: {key_body}"
    );
    let card: Value = serde_json::from_str(&key_body).expect("a served card is JSON");
    assert_eq!(
        card["name"], SHARED,
        "the served card is not the card of the key"
    );

    drop((first, second, host));
    let _ = std::fs::remove_file(&state_file);
}
