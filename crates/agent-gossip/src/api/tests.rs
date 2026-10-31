//! The topic path: an empty string is rejected, and two peers deriving the
//! same mesh from the same string converge and exchange.

use std::time::Duration;

use habilis_network::protocol::messages::Authored;
use habilis_network::protocol::{
    Mesh, MeshConfig, MeshName, MessageBody, Nickname, TransportPolicy,
};
use habilis_network::runtime::CoHostPolicy;
use tokio::sync::broadcast::Receiver;

use super::{CreateConfig, JoinError, MeshSession, TopicConfig};
use crate::a2a::status_update_params;

/// Wait for alice's topic message on `bob_rx`, ignoring anything else
/// (e.g. earlier retries' echoes) until the channel closes.
async fn recv_hello_topic(bob_rx: &mut Receiver<Authored>) -> bool {
    loop {
        match bob_rx.recv().await {
            Ok(message)
                if message.author.as_str() == "alice-topic"
                    && status_update_params::chat_text(&message).as_deref()
                        == Some("hello topic") =>
            {
                return true;
            }
            Ok(_) => {}
            Err(_) => return false,
        }
    }
}

/// The empty/whitespace-string guard is centralized in `TopicParams::resolve`,
/// so it holds for the public api too (not just the CLI/MCP edges) —
/// an empty string would otherwise join one globally-fixed mesh.
#[tokio::test]
async fn topic_rejects_empty_string() {
    for raw in ["", "   "] {
        let result = MeshSession::topic(TopicConfig::new(raw.to_owned())).await;
        assert!(
            matches!(result, Err(JoinError::Resolve(_))),
            "empty topic string must be rejected, got {:?}",
            result.map(|_| "Ok")
        );
    }
}

/// End-to-end convergence: two peers deriving from the *same string* land
/// in the same mesh and mesh. Loopback keeps the test hermetic; the seed +
/// name derivation and the `EagerProbed` first-peer beaconing are identical
/// to a real (public) `topic` — only the transport differs.
#[tokio::test]
async fn topic_peers_from_same_string_converge_and_exchange() {
    let topic = "agent-habilis";
    let first = Mesh::for_topic(topic, MeshConfig::loopback())
        .expect("the same topic string always derives a valid mesh id");
    let second = Mesh::for_topic(topic, MeshConfig::loopback())
        .expect("the same topic string always derives a valid mesh id");
    assert_eq!(
        first.to_string(),
        second.to_string(),
        "same string ⇒ same mesh id"
    );

    let alice = MeshSession::join_decoded(
        first,
        Some(Nickname::try_new("alice-topic").expect("the test nickname is valid")),
        CoHostPolicy::EagerProbed,
    )
    .await
    .expect("the first loopback joiner becomes the host and needs no other peer");
    let bob = MeshSession::join_decoded(
        second,
        Some(Nickname::try_new("bob-topic").expect("the test nickname is valid")),
        CoHostPolicy::EagerProbed,
    )
    .await
    .expect("the second loopback joiner finds the first one through its beacon");
    assert_eq!(alice.mesh_id(), bob.mesh_id(), "both derived the same id");

    let mut bob_rx = bob.messages();
    // Re-send until the loopback mesh forms; break the instant bob sees it.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut received = false;
    while !received && tokio::time::Instant::now() < deadline {
        alice
            .broadcast(MessageBody::from("hello topic"))
            .await
            .expect("alice send");
        let seen =
            tokio::time::timeout(Duration::from_millis(500), recv_hello_topic(&mut bob_rx)).await;
        received = matches!(seen, Ok(true));
    }
    assert!(
        received,
        "bob should receive alice's message over the topic mesh"
    );

    bob.leave().await.ok();
    alice.leave().await.ok();
}

/// The app default carries no multihop: a mesh that wants it names it.
#[test]
fn create_config_default_transport_is_udp_webrtc_gossip() {
    let config = CreateConfig::new(MeshName::try_new("defaults").expect("valid name"));
    assert_eq!(
        config.transport,
        TransportPolicy::default().multihop(false),
        "existing meshes only connect if the app default keeps udp and webrtc with gossip on"
    );
}

#[test]
fn create_config_builder_sets_only_the_named_fields() {
    let name = MeshName::try_new("builder").expect("valid name");
    let untouched = CreateConfig::new(name.clone());
    let config = CreateConfig::new(name)
        .with_max_peers(3)
        .with_password("secret".to_owned())
        .with_advertise(true);
    assert_eq!(config.max_peers, 3, "with_max_peers sets the cap");
    assert_eq!(
        config.password.as_deref(),
        Some("secret"),
        "the builder must store the given secret unchanged, or the mesh gate would reject every joiner"
    );
    assert!(
        config.advertise,
        "with_advertise(true) turns advertising on"
    );
    assert_eq!(
        config.transport, untouched.transport,
        "a field that no with_ call names keeps its default"
    );
    assert!(
        config.nickname.is_none(),
        "the nickname stays random when unset"
    );
}
