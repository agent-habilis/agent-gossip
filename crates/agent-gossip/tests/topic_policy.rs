//! The `topic_mdns_only` lane derives its topic mesh from the app policy
//! (`udp,webrtc,gossip`, no relay, no multihop), not from the engine default.
//!
//! Own integration binary: the process-global `Tuning` is a `OnceLock`, and
//! this suite needs `topic_mdns_only` on for the whole process.

use agent_gossip_test_fixtures::InProcNode;
use habilis_network::protocol::{LookupOpts, MeshConfig, RelayChoice, TransportPolicy};
use habilis_network::runtime::derive_topic_mesh_config;
use habilis_network::runtime::tuning::{Tuning, init};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mdns_only_topic_lane_uses_the_app_policy_without_relay() {
    let mut tuning = Tuning::DEFAULTS;
    tuning.topic_mdns_only = true;
    init(tuning);
    let topic = format!("topic-policy-{}", std::process::id());

    let node = InProcNode::topic(&topic, "policy-alice").await;

    let expected = derive_topic_mesh_config(
        &topic,
        MeshConfig::new(
            LookupOpts::new(RelayChoice::Disabled).with_mdns(true),
            None,
            None,
            TransportPolicy::default()
                .with_multihop(false)
                .with_relay_transport(false),
        ),
    )
    .expect("a non-empty string derives");
    assert_eq!(
        node.mesh,
        expected.to_string(),
        "the topic joiner lands on the gossip id derived from the string"
    );

    node.leave().await;
}
