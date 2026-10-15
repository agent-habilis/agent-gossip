use std::time::{Duration, Instant};

use agent_gossip_test_fixtures as common;
use common::{
    MSG_TIMEOUT, Node, POLL, await_peer_card_cli, cli_msg_checked, cli_peers, cli_poll, park_bell,
};

fn pair(name: &str) -> (Node, Node, String) {
    let (alice, mesh) = Node::create_named(name);
    let bob = Node::join(&mesh, &format!("{name}-bob"));
    assert!(alice.wait_ready(&mesh), "alice socket never appeared");
    assert!(bob.wait_ready(&mesh), "bob socket never appeared");
    await_peer_card_cli(&mesh, &alice.nickname, &bob.nickname);
    await_peer_card_cli(&mesh, &bob.nickname, &alice.nickname);
    (alice, bob, mesh)
}

fn msg_id(mesh: &str, from: &str, to: &str, body: &str) -> String {
    cli_msg_checked(mesh, from, to, body)
}

fn receipts(mesh: &str, nickname: &str, after: Option<&str>) -> Vec<(String, String)> {
    let events: Vec<serde_json::Value> =
        serde_json::from_str(&cli_poll(mesh, nickname, after)).expect("poll prints a JSON array");
    events
        .iter()
        .filter(|event| event["event"] == "receipt")
        .map(|event| {
            (
                event["ref"].as_str().unwrap_or_default().to_owned(),
                event["state"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

fn wait_for_read_receipt(mesh: &str, nickname: &str, id: &str) -> bool {
    let deadline = Instant::now() + MSG_TIMEOUT;
    while Instant::now() < deadline {
        if receipts(mesh, nickname, Some("0"))
            .iter()
            .any(|(seen, state)| seen == id && state == "read")
        {
            return true;
        }
        std::thread::sleep(POLL);
    }
    false
}

fn reader_of(mesh: &str, asker: &str, target: &str) -> Option<serde_json::Value> {
    let out: serde_json::Value =
        serde_json::from_str(&cli_peers(mesh, asker)).expect("peers prints JSON");
    out["peers"]
        .as_array()?
        .iter()
        .find(|row| row["nickname"] == target)?
        .get("reader")
        .cloned()
}

#[test]
fn polling_a_directed_msg_sends_the_author_a_read_receipt() {
    let (alice, bob, mesh) = pair("rl-read");
    let id = msg_id(&mesh, &alice.nickname, &bob.nickname, "ping");

    let served = cli_poll(&mesh, &bob.nickname, None);

    assert!(served.contains("ping"), "bob never got the msg: {served}");
    assert!(
        wait_for_read_receipt(&mesh, &alice.nickname, &id),
        "alice never got a read receipt for {id}"
    );
}

#[test]
fn a_long_poll_alone_sends_no_read_receipt() {
    let (alice, bob, mesh) = pair("rl-bell");
    // Serve the join backlog first so the bell parks. No directed msg exists yet.
    cli_poll(&mesh, &bob.nickname, None);
    let mut bell = park_bell(&mesh, &bob.nickname);
    let id = msg_id(&mesh, &alice.nickname, &bob.nickname, "ping");
    // The directed msg rings the bell. A bell is not a reader.
    common::wait_bell_output(&mut bell);

    std::thread::sleep(Duration::from_secs(3));

    assert!(
        !receipts(&mesh, &alice.nickname, Some("0"))
            .iter()
            .any(|(seen, state)| *seen == id && state == "read"),
        "a bell must not send a read receipt"
    );
}

#[test]
fn a_re_read_with_after_sends_no_second_read_receipt() {
    let (alice, bob, mesh) = pair("rl-reread");
    let id = msg_id(&mesh, &alice.nickname, &bob.nickname, "ping");
    cli_poll(&mesh, &bob.nickname, None);
    assert!(wait_for_read_receipt(&mesh, &alice.nickname, &id));

    cli_poll(&mesh, &bob.nickname, Some("0"));
    std::thread::sleep(Duration::from_secs(3));

    let reads = receipts(&mesh, &alice.nickname, Some("0"))
        .into_iter()
        .filter(|(seen, state)| *seen == id && state == "read")
        .count();
    assert_eq!(reads, 1, "a re-read must not receipt twice");
}

#[test]
fn peers_shows_the_bell_and_the_last_poll_of_a_reader() {
    let (alice, bob, mesh) = pair("rl-peers");
    cli_poll(&mesh, &bob.nickname, None);
    let mut bell = park_bell(&mesh, &bob.nickname);

    let deadline = Instant::now() + MSG_TIMEOUT;
    let mut seen = None;
    while Instant::now() < deadline {
        seen = reader_of(&mesh, &alice.nickname, &bob.nickname)
            .filter(|reader| reader["bell"] == true);
        if seen.is_some() {
            break;
        }
        std::thread::sleep(POLL);
    }
    bell.kill().expect("stop the bell");
    bell.wait().expect("reap the bell");

    let seen = seen.expect("alice never saw bob's bell armed");
    let ago = seen["last_poll_secs_ago"].as_u64().expect("secs ago");
    assert!(ago < 180, "last poll should be recent, got {ago}s");
}
