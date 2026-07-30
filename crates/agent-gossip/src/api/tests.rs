//! The topic path: an empty string is rejected, and two peers deriving the
//! same mesh from the same string converge and exchange.

use std::time::Duration;

use agent_habilis_mesh::protocol::{Mesh, MeshConfig};
use agent_habilis_mesh::protocol::{Message, MessageBody, Nickname};
use agent_habilis_mesh::runtime::CoHostPolicy;
use tokio::sync::broadcast;

use super::{JoinError, MeshSession, TopicConfig};

/// Wait for alice's topic message on `bob_rx`, ignoring anything else
/// (e.g. earlier retries' echoes) until the channel closes.
async fn recv_hello_topic(bob_rx: &mut broadcast::Receiver<Message>) -> bool {
    loop {
        match bob_rx.recv().await {
            Ok(msg)
                if msg.author.as_str() == "alice-topic"
                    && crate::a2a::gossip::chat_text(&msg).as_deref() == Some("hello topic") =>
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
    let first = Mesh::from_topic(topic, MeshConfig::loopback());
    let second = Mesh::from_topic(topic, MeshConfig::loopback());
    assert_eq!(
        first.to_string(),
        second.to_string(),
        "same string ⇒ same mesh id"
    );

    let alice = MeshSession::join_decoded(
        first,
        Some(Nickname::new("alice-topic").unwrap()),
        CoHostPolicy::EagerProbed,
    )
    .await
    .expect("alice topic session");
    let bob = MeshSession::join_decoded(
        second,
        Some(Nickname::new("bob-topic").unwrap()),
        CoHostPolicy::EagerProbed,
    )
    .await
    .expect("bob topic session");
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
