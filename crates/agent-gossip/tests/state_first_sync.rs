//! A joiner that writes state at once meets the first-sync wait of the engine: the state refuses
//! its write with `not_synced` until a peer answers its first request. The harness waits for that
//! answer, as a user does when the error says to retry. Any other refusal still fails at once.

use agent_gossip_test_fixtures::InProcNode;
use serde_json::{Map, Value};

/// `{key: value}` for a key computed at run time.
fn one(key: String, value: Value) -> Value {
    let mut map = Map::new();
    map.insert(key, value);
    Value::Object(map)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn joiners_that_write_state_at_once_wait_for_their_first_sync() {
    let creator = InProcNode::create("first-sync-writer").await;
    for round in 0..6 {
        let joiner = InProcNode::join(&creator.mesh, &format!("fs-joiner-{round}")).await;
        joiner
            .state_merge(one(format!("k{round}"), Value::from(round)))
            .await;
        let document = joiner.state_get().await;
        assert_eq!(
            document[format!("k{round}")],
            Value::from(round),
            "the write of the joiner is in its document"
        );
        joiner.leave().await;
    }
    creator.leave().await;
}

/// Only `not_synced` is waited for: a refusal that more waiting cannot fix fails at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[should_panic(expected = "nested 13 levels deep")]
async fn another_refusal_fails_the_write_at_once() {
    let creator = InProcNode::create("first-sync-other").await;
    let deep = (0..13).fold(Value::from(1), |inner, _| one("a".to_owned(), inner));
    creator.state_merge(deep).await;
}
