//! Two members claim one nickname with two different member keys. The engine gate refuses the
//! frames of the other member, and the node reports it as the `nickname_held` event: visible, and it
//! rings the bell (the unit test `a_nickname_held_event_is_pollable_and_wakes_the_bell` pins the ring).

use std::time::Instant;

use agent_gossip_test_fixtures::{InProcNode, MSG_TIMEOUT, POLL};
use serde_json::Value;

const SHARED: &str = "held-dup";

/// The `nickname_held` events of `node`, as the documented JSON lines.
fn held_events(node: &mut InProcNode) -> Vec<Value> {
    node.json_events()
        .filter(|event| event["event"] == "nickname_held")
        .collect()
}

/// Wait until `node` has a `nickname_held` event.
async fn await_held(node: &mut InProcNode) -> Vec<Value> {
    let deadline = Instant::now() + MSG_TIMEOUT;
    loop {
        let events = held_events(node);
        if !events.is_empty() {
            return events;
        }
        assert!(
            Instant::now() < deadline,
            "{} never got a nickname_held event",
            node.nickname
        );
        tokio::time::sleep(POLL).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_member_that_is_refused_its_nickname_gets_one_visible_nickname_held_event() {
    let first = InProcNode::create_with_nick("nickname-held", SHARED).await;
    let mut second = InProcNode::join(&first.mesh, SHARED).await;

    let held = await_held(&mut second).await;

    assert_eq!(held.len(), 1, "the event comes once: {held:?}");
    assert_eq!(held[0]["nickname"], SHARED, "it names the held nickname");
    assert_eq!(
        held[0]["is_visible"], true,
        "the event is visible, so a skill prints it"
    );

    first.leave().await;
    second.leave().await;
}

/// The wanted rule: the member that proved its nickname first holds it and gets no event. At
/// engine `0153746c` the gate tells both members (a proof of another member under our nickname is
/// the join error, whoever proved first), so this test fails there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "engine 0153746c reports the held nickname to both members"]
async fn the_member_that_holds_the_nickname_gets_no_event() {
    let mut first = InProcNode::create_with_nick("nickname-held-first", SHARED).await;
    let mut second = InProcNode::join(&first.mesh, SHARED).await;
    await_held(&mut second).await;

    let held = held_events(&mut first);
    assert!(held.is_empty(), "the holder got an event: {held:?}");

    first.leave().await;
    second.leave().await;
}
