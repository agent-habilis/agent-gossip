use std::time::{Duration, Instant};

use agent_gossip_test_fixtures as common;
use common::{
    MSG_TIMEOUT, Node, POLL, assert_bell_parked, await_peer_card_cli, cli_msg_checked, cli_peers,
    cli_poll, park_bell,
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

fn wait_for_reader(mesh: &str, asker: &str, target: &str) -> Option<serde_json::Value> {
    let deadline = Instant::now() + MSG_TIMEOUT;
    while Instant::now() < deadline {
        // The first call after a join can miss the key while a cold dial runs.
        if let Some(reader) = reader_of(mesh, asker, target).filter(|reader| reader["bell"] == true)
        {
            return Some(reader);
        }
        std::thread::sleep(POLL);
    }
    None
}

#[test]
fn peers_shows_the_bell_and_the_last_poll_of_a_reader() {
    let (alice, bob, mesh) = pair("rl-peers");
    cli_poll(&mesh, &bob.nickname, None);
    let mut bell = park_bell(&mesh, &bob.nickname);

    let seen = wait_for_reader(&mesh, &alice.nickname, &bob.nickname);
    bell.kill().expect("stop the bell");
    bell.wait().expect("reap the bell");

    let seen = seen.expect("alice never saw bob's bell armed");
    let ago = seen["last_poll_secs_ago"].as_u64().expect("secs ago");
    assert!(ago < 60, "last poll should be recent, got {ago}s");
}

#[test]
fn the_reader_query_leaves_the_bell_parked_and_the_poll_unchanged() {
    let (alice, bob, mesh) = pair("rl-quiet");
    cli_poll(&mesh, &bob.nickname, None);
    let mut bell = park_bell(&mesh, &bob.nickname);
    let before = poll_len(&mesh, &bob.nickname);

    let seen = wait_for_reader(&mesh, &alice.nickname, &bob.nickname);

    assert!(seen.is_some(), "alice never saw bob's bell armed");
    assert_bell_parked(&mut bell, "a reader query must not ring the bell");
    assert_eq!(
        poll_len(&mesh, &bob.nickname),
        before,
        "a reader query must add no event to bob's poll"
    );
    bell.kill().expect("stop the bell");
    bell.wait().expect("reap the bell");
}

#[test]
fn a_peer_that_does_not_answer_gets_no_reader_and_peers_stays_fast() {
    let (alice, bob, mesh) = pair("rl-mute");
    bob.stop();

    let started = Instant::now();
    let reader = reader_of(&mesh, &alice.nickname, &bob.nickname);
    let took = started.elapsed();
    bob.cont();

    assert_eq!(reader, None, "a frozen peer must get no reader key");
    assert!(
        took < Duration::from_secs(2),
        "peers must not wait on a mute peer, took {took:?}"
    );
}

fn poll_len(mesh: &str, nickname: &str) -> usize {
    let events: Vec<serde_json::Value> = serde_json::from_str(&cli_poll(mesh, nickname, Some("0")))
        .expect("poll prints a JSON array");
    events.len()
}
