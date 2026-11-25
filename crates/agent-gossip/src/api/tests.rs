//! The topic path: an empty string is rejected, and two peers deriving the
//! same mesh from the same string converge and exchange.

use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::time::Duration;

use habilis_network::protocol::messages::Authored;
use habilis_network::protocol::{
    LookupSet, Mesh, MeshConfig, MeshName, MessageBody, Nickname, TransportPolicy,
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
        crate::runtime_base(),
    )
    .await
    .expect("the first loopback joiner becomes the host and needs no other peer");
    let bob = MeshSession::join_decoded(
        second,
        Some(Nickname::try_new("bob-topic").expect("the test nickname is valid")),
        CoHostPolicy::EagerProbed,
        crate::runtime_base(),
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

/// A directory session never takes its event stream, so it must not hold one:
/// every event it captured would sit in memory for the life of the process.
#[tokio::test]
async fn directory_session_holds_no_event_stream() {
    let mesh = Mesh::for_topic("directory-no-capture", MeshConfig::loopback())
        .expect("the test topic derives a valid mesh id");
    let mut session = MeshSession::join_decoded(
        mesh,
        Some(Nickname::try_new("dir-quiet").expect("the test nickname is valid")),
        CoHostPolicy::Never,
        crate::runtime_base(),
    )
    .await
    .expect("a loopback directory session joins alone");

    assert!(
        session.events().is_none(),
        "a directory session must not capture events"
    );

    session.leave().await.ok();
}

/// A runtime base that this test made, with mode 0700: a name that exists already is not taken, so a
/// folder that a crashed run left, or that an other user made first, is never reused.
fn fresh_base(label: &str) -> PathBuf {
    let mut names = (0..64).map(|number| {
        std::env::temp_dir().join(format!("agp-{label}-{}-{number}", std::process::id()))
    });
    let made = |path: &PathBuf| DirBuilder::new().mode(0o700).create(path).is_ok();
    names
        .find(made)
        .expect("a free folder name exists within 64 tries")
}

/// The number of runtime records of a base: 1 file for each session that holds a lock there.
fn lock_count(base: &std::path::Path) -> usize {
    std::fs::read_dir(base.join("locks")).map_or(0, Iterator::count)
}

/// The base that a caller names is the base of a directory session too. A directory session that took
/// the base of the host would leave its files where the caller did not look, and where the leave of the
/// caller would not find them.
#[tokio::test]
async fn a_directory_session_writes_its_runtime_files_under_the_base_it_is_given() {
    let base = fresh_base("dir-base");
    let mesh = Mesh::for_topic("directory-named-base", MeshConfig::loopback())
        .expect("the test topic derives a valid mesh id");
    let session = MeshSession::join_decoded(
        mesh,
        Some(Nickname::try_new("dir-based").expect("the test nickname is valid")),
        CoHostPolicy::Never,
        base.clone(),
    )
    .await
    .expect("a loopback directory session joins alone");

    // The session takes its lock when its loop starts, a moment after `join_decoded` returns.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while lock_count(&base) == 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let written = lock_count(&base);
    session.leave().await.ok();
    std::fs::remove_dir_all(&base).ok();

    assert_eq!(
        written, 1,
        "the session wrote its lock under the base it was given"
    );
}

/// The wiring of the advertiser: a mesh that is created with a named base and listed in a directory holds
/// 2 sessions, the mesh and the directory session of its advertiser, and both write under that base.
/// The advertiser needs a reachable lookup, so the test names mDNS: the sessions open no connection
/// before they hold their locks.
#[tokio::test]
async fn an_advertised_mesh_writes_the_files_of_its_advertiser_under_its_base() {
    let base = fresh_base("advertise-base");
    let mut lookups = LookupSet::default();
    lookups.mdns = true;
    let name = MeshName::try_new("advertised-base").expect("valid name");
    let nickname = Nickname::try_new("adv-based").expect("the test nickname is valid");
    let config = CreateConfig::new(name)
        .with_nickname(nickname)
        .with_lookups(lookups)
        .with_advertise(true);
    let session = MeshSession::create_with_base(config, base.clone())
        .await
        .expect("a mesh with an mDNS lookup can advertise");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while lock_count(&base) < 2 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let written = lock_count(&base);
    session.leave().await.ok();
    std::fs::remove_dir_all(&base).ok();

    assert_eq!(
        written, 2,
        "the mesh and its advertiser both wrote their locks under the base of the call"
    );
}

/// The app default carries multihop and no gossip: a mesh that wants gossip names it.
#[test]
fn create_config_default_transport_is_udp_webrtc_multihop() {
    let config = CreateConfig::new(MeshName::try_new("defaults").expect("valid name"));
    assert_eq!(
        config.transport,
        TransportPolicy::default().relay_transport(false),
        "a loopback create takes udp, webrtc and multihop, with no relay and no gossip"
    );
}

/// A browser tab holds 8; every native config starts from the same default.
#[test]
fn every_config_defaults_to_16_direct_connections() {
    let name = MeshName::try_new("capdefault").expect("valid name");
    assert_eq!(
        CreateConfig::new(name).max_connections,
        16,
        "a create holds 16 direct connections by default"
    );
    assert_eq!(
        TopicConfig::new("capdefault".to_owned()).max_connections,
        16,
        "a topic holds 16 direct connections by default"
    );
}

#[test]
fn create_config_builder_sets_only_the_named_fields() {
    let name = MeshName::try_new("builder").expect("valid name");
    let untouched = CreateConfig::new(name.clone());
    let config = CreateConfig::new(name)
        .with_max_connections(3)
        .with_password("secret".to_owned())
        .with_advertise(true);
    assert_eq!(
        config.max_connections, 3,
        "with_max_connections sets the cap"
    );
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
