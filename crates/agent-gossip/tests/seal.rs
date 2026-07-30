//! End-to-end sealing (in-process, via the `InProcNode` harness): a directed
//! A2A frame is encrypted to its recipient, so a third peer (a relay) forwards
//! it but never surfaces its content, while the addressee decrypts it through
//! the real event loop. Broadcast stays public — every member reads it. The
//! sealed-box crypto itself (round-trip, wrong key, tamper) is unit-tested in
//! `src/protocol/seal.rs`.

use agent_gossip_test_fixtures as common;

use std::time::Duration;

use common::{InProcNode, MSG_TIMEOUT};

/// A directed task A→B is readable only by B; the relay C never surfaces it.
/// Broadcast from A reaches both B and C.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn directed_frames_are_private_to_the_recipient_broadcast_stays_public() {
    let alice = InProcNode::create("seal").await;
    let mut bob = InProcNode::join(&alice.mesh, "seal-bob").await;
    let mut carol = InProcNode::join(&alice.mesh, "seal-carol").await;

    // Warm the mesh; a broadcast reaches everyone (proves C is a live relay).
    alice.broadcast("warmup").await;
    assert!(bob.wait_body("warmup", MSG_TIMEOUT).await, "bob meshed");
    assert!(carol.wait_body("warmup", MSG_TIMEOUT).await, "carol meshed");

    // A delegates a task to B — a directed, sealed `message/send`.
    let resp = alice
        .create_task("seal-bob", "the private brief for bob")
        .await;
    assert!(resp["result"]["task"].is_object(), "task created: {resp}");

    // B is the addressee: it decrypts the sealed brief and surfaces it.
    assert!(
        bob.wait_task_message(MSG_TIMEOUT).await,
        "bob must decrypt and surface the directed task brief"
    );
    // C relays the same frame but cannot read the sealed body, so it never
    // surfaces the A→B task.
    assert!(
        !carol.wait_task_message(Duration::from_secs(2)).await,
        "carol (a relay) must not surface a task it is not addressed by"
    );

    // Broadcast stays public: A's chat is readable by every member, C included.
    alice.broadcast("a public announcement").await;
    assert!(
        bob.wait_body("a public announcement", MSG_TIMEOUT).await,
        "bob reads broadcast"
    );
    assert!(
        carol.wait_body("a public announcement", MSG_TIMEOUT).await,
        "carol reads broadcast — broadcast is not sealed"
    );

    alice.leave().await;
    bob.leave().await;
    carol.leave().await;
}

/// The privacy contract for chat: a `msg` A→B is surfaced by exactly its two
/// parties. B renders it, A sees its own echo, and C — a live relay that
/// carries the frame — never surfaces it at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_msg_is_surfaced_only_by_its_two_parties() {
    const SECRET: &str = "just between us";

    let mut alice = InProcNode::create("msgpriv").await;
    let mut bob = InProcNode::join(&alice.mesh, "msgpriv-bob").await;
    let mut carol = InProcNode::join(&alice.mesh, "msgpriv-carol").await;

    // Warm the mesh; a broadcast reaching everyone proves C is a live relay,
    // so its silence later is about the msg and not about a dead link.
    alice.broadcast("warmup").await;
    assert!(bob.wait_body("warmup", MSG_TIMEOUT).await, "bob meshed");
    assert!(carol.wait_body("warmup", MSG_TIMEOUT).await, "carol meshed");

    // A msg is sealed to its recipient, so the card must have replicated.
    alice.await_peer_card("msgpriv-bob").await;

    alice.msg("msgpriv-bob", SECRET).await;

    // B is the addressee: it decrypts and renders the msg as a chat line.
    assert!(
        bob.wait_body(SECRET, MSG_TIMEOUT).await,
        "the addressee must surface the msg"
    );
    let received = bob
        .msg_events()
        .into_iter()
        .find(|event| event["body"] == SECRET)
        .expect("bob surfaced the msg as a chat event");
    assert_eq!(received["type"], "msg");
    assert_eq!(received["to"], "msgpriv-bob", "the msg names its recipient");
    assert_eq!(received["self"], false);
    assert_eq!(
        received["is_visible"], true,
        "a msg is printed, not context"
    );
    assert_eq!(
        received["display"],
        format!("💬️ `<{}>` → `<msgpriv-bob>`: {SECRET}", alice.nickname),
        "the arrow form is what marks a line private on screen"
    );

    // A sees its own echo — the send confirmation.
    let echoed = alice
        .msg_events()
        .into_iter()
        .find(|event| event["body"] == SECRET)
        .expect("the sender surfaced its own msg echo");
    assert_eq!(echoed["self"], true);
    assert_eq!(echoed["to"], "msgpriv-bob");

    // C relays the frame but is not a party to it. Assert on the *content*
    // rather than on an empty window: presence beats and a late copy of the
    // warmup broadcast legitimately land here, and asserting emptiness would
    // fail the leg for the wrong reason.
    let carol_saw_secret = carol
        .message_events()
        .into_iter()
        .any(|event| event["body"] == SECRET);
    assert!(
        !carol_saw_secret,
        "a relay must never surface a msg it is not party to"
    );

    // Belt and braces: the same holds after giving the frame time to arrive.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let carol_saw_secret_later = carol
        .message_events()
        .into_iter()
        .any(|event| event["body"] == SECRET);
    assert!(
        !carol_saw_secret_later,
        "the msg must not surface on a relay even after it has settled"
    );

    alice.leave().await;
    bob.leave().await;
    carol.leave().await;
}

/// A msg must not join the per-author hash chain. It reaches only its
/// addressee, so a chain entry for it is an unfillable gap for every other
/// peer — which fork detection reads as a fork. Guards the `chained` repoint
/// in `classify`, which no existing assertion catches.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn msgs_do_not_advance_the_chain_or_fork_the_gossip() {
    let alice = InProcNode::create("msgchain").await;
    let mut bob = InProcNode::join(&alice.mesh, "msgchain-bob").await;
    let mut carol = InProcNode::join(&alice.mesh, "msgchain-carol").await;

    alice.broadcast("warmup").await;
    assert!(bob.wait_body("warmup", MSG_TIMEOUT).await, "bob meshed");
    assert!(carol.wait_body("warmup", MSG_TIMEOUT).await, "carol meshed");
    alice.await_peer_card("msgchain-bob").await;

    // A burst of msgs C cannot see. If these were chained, C would observe a
    // run of missing seqs from A.
    for index in 0..5 {
        alice.msg("msgchain-bob", &format!("private {index}")).await;
    }
    assert!(
        bob.wait_body("private 4", MSG_TIMEOUT).await,
        "bob received the burst"
    );

    // Broadcast still flows to the peer that saw none of the burst, and no
    // fork is reported anywhere.
    alice.broadcast("after the burst").await;
    assert!(
        carol.wait_body("after the burst", MSG_TIMEOUT).await,
        "a peer blind to the msgs still accepts the next broadcast — the \
         chain never advanced for them"
    );
    for (node, who) in [(&mut bob, "bob"), (&mut carol, "carol")] {
        let forks = node
            .json_events()
            .into_iter()
            .filter(|event| event["event"] == "fork")
            .count();
        assert_eq!(forks, 0, "{who} must not see a fork");
    }

    alice.leave().await;
    bob.leave().await;
    carol.leave().await;
}
