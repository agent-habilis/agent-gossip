#![expect(
    missing_docs,
    reason = "an integration test crate has no public docs; the module path names it"
)]

use agent_gossip_test_fixtures::InProcNode;

/// The runtime base of a node goes with its session. A test that takes the session out of the node
/// must not leave the base to be removed under the running session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_that_leaves_its_node_takes_the_runtime_base_with_it() {
    let node = InProcNode::create("own-base-moved").await;
    let base = node.session.base().to_path_buf();
    assert!(base.exists(), "the node made its base when it started");

    let session = node.session;

    assert!(
        base.exists(),
        "the node was dropped, and the base of the session that left it is still there"
    );
    assert!(
        session.peers().await.is_ok(),
        "the session still runs after its node is gone"
    );
    session
        .leave()
        .await
        .expect("the loop of the session runs until it leaves");
    assert!(
        !base.exists(),
        "the base goes when the session that holds it is gone"
    );
}
