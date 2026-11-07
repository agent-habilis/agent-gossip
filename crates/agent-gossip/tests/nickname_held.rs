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

/// The rule is symmetric on purpose: a tie-break needs an input that the claimant chooses (an older
/// proof time, a lower key), so a forger would win it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn both_members_of_one_nickname_get_one_visible_nickname_held_event() {
    let mut first = InProcNode::create_with_nick("nickname-held", SHARED).await;
    let mut second = InProcNode::join(&first.mesh, SHARED).await;

    for node in [&mut first, &mut second] {
        let held = await_held(node).await;
        assert_eq!(held.len(), 1, "the event comes once: {held:?}");
        assert_eq!(held[0]["nickname"], SHARED, "it names the held nickname");
        assert_eq!(
            held[0]["is_visible"], true,
            "the event is visible, so a skill prints it"
        );
    }

    first.leave().await;
    second.leave().await;
}
