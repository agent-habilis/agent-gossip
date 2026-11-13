//! A joiner can write state at once: the engine has no first-sync wait, so no write is refused
//! with `not_synced` and the harness has no retry for it.

use agent_gossip_test_fixtures::InProcNode;
use habilis_network::protocol::AuthorKey;
use serde_json::{Map, Value};

/// `{key: value}` for a key computed at run time.
fn one(key: String, value: Value) -> Value {
    let mut map = Map::new();
    map.insert(key, value);
    Value::Object(map)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn joiners_that_write_state_at_once_find_the_write_in_their_view() {
    let creator = InProcNode::create("first-sync-writer").await;
    for round in 0..6 {
        let joiner = InProcNode::join(&creator.mesh, &format!("fs-joiner-{round}")).await;
        let own: AuthorKey = joiner.key_seen_by(&creator).await;
        joiner
            .state_merge(one(format!("k{round}"), Value::from(round)))
            .await;
        let view = joiner.state_get().await;
        assert_eq!(
            view[own.to_string()][format!("k{round}")],
            Value::from(round),
            "the write of the joiner is in its own slice of the view"
        );
        joiner.leave().await;
    }
    creator.leave().await;
}
