//! Shared-state behavioral tests, in-process over the real event loop and a real
//! iroh mesh (`common::InProcNode`). They exercise the whole primitive — apply an
//! RFC 7386 merge → fold the log → derive the document → surface the change →
//! react → merge back — with no app logic in the daemon.
//!
//! Every behavior runs against **both channels** (`state` and `meta`): each test
//! body is `*_for(channel)`, with thin per-channel `#[tokio::test]` wrappers
//! (`state_*` / `meta_*`). The two channels share one code path parameterized by
//! `Channel`, so this proves parity by construction — a regression on either
//! channel turns exactly one named test red.
//!
//! What each behavior pins:
//! - convergence: the fold is a deterministic function of the event *set* (for
//!   disjoint keys merge is order-independent);
//! - RFC 7386 semantics: a non-object merge replaces the whole document;
//! - the reaction hook (F8) and the self-wake guard (F5): a peer's change wakes
//!   an agent with the derived document, its own change does not;
//! - the unbounded log + windowed anti-entropy: a late joiner reconciles a log
//!   far larger than one digest window;
//! - reaction + convergence end-to-end: two agents ping-pong a counter via the
//!   shared document and both converge, strictly alternating (no double-move).
//!
//! `meta_and_state_channels_are_independent` is the one inherently cross-channel
//! test and stays standalone.

use agent_gossip_test_fixtures as common;

use std::time::{Duration, Instant};

use agent_gossip::Channel;
use common::{InProcNode, MSG_TIMEOUT, POLL, RECOVERY_TIMEOUT};
use serde_json::{Value, json};

/// `state` / `meta`, for naming meshes and assert messages (the crate's
/// `Channel::label` is `pub(crate)`, not reachable from this test crate).
fn label(channel: Channel) -> &'static str {
    match channel {
        Channel::State => "state",
        Channel::Meta => "meta",
    }
}

/// A per-channel mesh name so the `state` and `meta` variants of one behavior
/// never share a name when they run concurrently.
fn mesh_name(channel: Channel, base: &str) -> String {
    format!("{base}-{}", label(channel))
}

/// A single-key merge object `{key: value}` — for keys computed at runtime, which
/// the `json!` macro can't take as a literal key.
fn one(key: String, value: Value) -> Value {
    let mut map = serde_json::Map::new();
    map.insert(key, value);
    Value::Object(map)
}

/// The application's view of a channel document: the daemon-published
/// `AgentCard`s (meta `/peers/<nick>/card`, written once per member at join)
/// removed, since these tests pin app-driven merges. The card publication has
/// its own test (`agent_cards_publish_to_meta_on_join`).
fn app_view(doc: &Value) -> Value {
    let mut doc = doc.clone();
    if let Some(peers) = doc.get_mut("peers").and_then(Value::as_object_mut) {
        for entry in peers.values_mut() {
            if let Some(obj) = entry.as_object_mut() {
                obj.remove("card");
            }
        }
        peers.retain(|_, entry| entry.as_object().is_none_or(|obj| !obj.is_empty()));
    }
    if doc
        .get("peers")
        .and_then(Value::as_object)
        .is_some_and(serde_json::Map::is_empty)
        && let Some(map) = doc.as_object_mut()
    {
        map.remove("peers");
    }
    doc
}

/// Poll a node's derived document for `channel` until `pred` holds or `timeout`
/// elapses.
async fn wait_doc(
    node: &InProcNode,
    channel: Channel,
    timeout: Duration,
    mut pred: impl FnMut(&Value) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if pred(&node.get(channel).await) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The `state` and `meta` channels are fully independent: a write to one
/// propagates on its own log and leaves the other untouched (own doc, own hash).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn meta_and_state_channels_are_independent() {
    let alice = InProcNode::create("ch-indep").await;
    let mut bob = InProcNode::join(&alice.mesh, "ch-bob").await;
    alice.send("link").await;
    assert!(bob.wait_body("link", MSG_TIMEOUT).await, "bob meshed");

    // A meta write propagates to bob's meta doc; both state docs stay empty.
    alice
        .meta_merge(json!({"peers": {"alice": {"model": "Opus 4.8"}}}))
        .await;
    assert!(
        wait_doc(&bob, Channel::Meta, RECOVERY_TIMEOUT, |doc| {
            doc.pointer("/peers/alice/model") == Some(&json!("Opus 4.8"))
        })
        .await,
        "bob never saw the meta write: {}",
        bob.meta_get().await
    );
    assert_eq!(
        alice.state_get().await,
        json!({}),
        "meta write left state empty"
    );
    assert_eq!(
        bob.state_get().await,
        json!({}),
        "meta write left state empty"
    );

    // A state write propagates to bob's state doc; meta is unchanged by it.
    alice.state_merge(json!({"turn": "a"})).await;
    assert!(
        wait_doc(&bob, Channel::State, RECOVERY_TIMEOUT, |doc| doc
            .pointer("/turn")
            == Some(&json!("a")))
        .await,
        "bob never saw the state write: {}",
        bob.state_get().await
    );
    // meta still holds only the earlier write (plus the daemon-published
    // cards, masked by `app_view`).
    assert_eq!(
        app_view(&alice.meta_get().await),
        json!({"peers": {"alice": {"model": "Opus 4.8"}}}),
        "state write must not touch meta"
    );

    alice.leave().await;
    bob.leave().await;
}

/// Three peers each apply an independent merge; whatever order they arrive in,
/// every peer folds the same event *set* into the byte-identical document. Proves
/// the reducer is set-deterministic for disjoint keys (the convergence property).
async fn patches_converge_for(channel: Channel) {
    let alice = InProcNode::create(&mesh_name(channel, "ss-conv")).await;
    let mut bob = InProcNode::join(&alice.mesh, "conv-bob").await;
    let mut carol = InProcNode::join(&alice.mesh, "conv-carol").await;

    // Mesh first (a delivered message proves the links).
    alice.send("link").await;
    assert!(bob.wait_body("link", MSG_TIMEOUT).await, "bob meshed");
    assert!(carol.wait_body("link", MSG_TIMEOUT).await, "carol meshed");

    // Each peer seeds a distinct key concurrently — intermediate states differ
    // by arrival order, the final set does not.
    alice.merge(channel, json!({"a": 1})).await;
    bob.merge(channel, json!({"b": 2})).await;
    carol.merge(channel, json!({"c": 3})).await;

    let want = json!({"a": 1, "b": 2, "c": 3});
    for (node, who) in [(&alice, "alice"), (&bob, "bob"), (&carol, "carol")] {
        assert!(
            wait_doc(node, channel, RECOVERY_TIMEOUT, |doc| app_view(doc) == want).await,
            "{who} never converged on {}: {}",
            label(channel),
            node.get(channel).await
        );
    }

    alice.leave().await;
    bob.leave().await;
    carol.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn state_patches_converge_to_identical_documents() {
    patches_converge_for(Channel::State).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn meta_patches_converge_to_identical_documents() {
    patches_converge_for(Channel::Meta).await;
}

/// The reaction hook (F8) and the self-wake guard (F5): a peer's merge wakes an
/// agent on its `events()` channel carrying the freshly-derived document, while
/// the author is **not** woken on its own channel for its own merge.
async fn peer_change_wakes_for(channel: Channel) {
    let mut alice = InProcNode::create(&mesh_name(channel, "ss-wake")).await;
    let mut bob = InProcNode::join(&alice.mesh, "wake-bob").await;

    alice.send("link").await;
    assert!(bob.wait_body("link", MSG_TIMEOUT).await, "bob meshed");

    alice.merge(channel, json!({"turn": "bob"})).await;

    // F8: bob is woken with the document.
    assert!(
        bob.wait_change(channel, MSG_TIMEOUT, |doc| doc["turn"] == "bob")
            .await,
        "a peer's {} change must wake the agent with the derived document (F8)",
        label(channel)
    );
    // F5: alice was never woken by her OWN merge (peers' card publications
    // legitimately wake her with `self:false`, so filter on the flag).
    let self_wakes: Vec<_> = alice
        .changes(channel)
        .into_iter()
        .filter(|(_, is_self)| *is_self)
        .collect();
    assert!(
        self_wakes.is_empty(),
        "an agent must not be woken on its own {} merge (F5), got {self_wakes:?}",
        label(channel)
    );

    alice.leave().await;
    bob.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_peer_change_wakes_the_agent_self_change_does_not() {
    peer_change_wakes_for(Channel::State).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn meta_peer_change_wakes_the_agent_self_change_does_not() {
    peer_change_wakes_for(Channel::Meta).await;
}

/// Unbounded log + windowed anti-entropy: drive far more merges than one digest
/// window holds, then a late joiner — arriving after all traffic, so only
/// anti-entropy can reach it — reconciles the *whole* log across several windows
/// and derives the identical document.
///
/// Each merge sets an independent key (`/k0`, `/k1`, …), so the *set* of events
/// fully determines the document regardless of replay order — the property under
/// test is completeness of backfill, not causal ordering.
async fn late_joiner_backfills_for(channel: Channel) {
    // Comfortably more than two digest windows (`ANTIENTROPY_DIGEST_WINDOW_IDS`
    // is 70), so the rolling older-window cursor is genuinely exercised.
    const PATCHES: usize = 160;

    let alice = InProcNode::create(&mesh_name(channel, "ss-backfill")).await;
    let early = InProcNode::join(&alice.mesh, "bf-early").await;
    // Mesh so the appends go out live (the creator's outbound buffer stays
    // empty) — the late joiner can then only be served by anti-entropy.
    alice.send("link").await;

    for index in 0..PATCHES {
        alice
            .merge(channel, one(format!("k{index}"), json!(index)))
            .await;
    }

    let want = app_view(&alice.get(channel).await);
    assert_eq!(
        want.as_object().map(serde_json::Map::len),
        Some(PATCHES),
        "creator's own {} document holds every key it set",
        label(channel)
    );

    // The early (meshed) peer converges via the live path.
    assert!(
        wait_doc(&early, channel, RECOVERY_TIMEOUT, |doc| app_view(doc)
            == want)
        .await,
        "early peer never converged on the live {} path",
        label(channel)
    );

    // The late joiner arrives after all traffic — windowed anti-entropy, across
    // multiple rounds, must reconstruct the full log.
    let late = InProcNode::join(&alice.mesh, "bf-late").await;
    assert!(
        wait_doc(&late, channel, Duration::from_secs(150), |doc| app_view(
            doc
        ) == want)
        .await,
        "late joiner never backfilled the full {} log via windowed anti-entropy ({} of {PATCHES} keys)",
        label(channel),
        app_view(&late.get(channel).await)
            .as_object()
            .map_or(0, serde_json::Map::len)
    );

    alice.leave().await;
    early.leave().await;
    late.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn state_late_joiner_backfills_a_log_larger_than_one_window() {
    late_joiner_backfills_for(Channel::State).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn meta_late_joiner_backfills_a_log_larger_than_one_window() {
    late_joiner_backfills_for(Channel::Meta).await;
}

/// The number of moves recorded in the shared document (one `/m{k}` key
/// each; the daemon-published cards are masked out).
fn move_count(doc: &Value) -> usize {
    app_view(doc).as_object().map_or(0, serde_json::Map::len)
}

/// Reaction + convergence end-to-end (the chess narrative, no engine): two
/// agents ping-pong, each reacting to the other's change before making its own.
/// Every move records itself under its OWN key (`/m1`, `/m2`, …) — an
/// independent set, with no dependency on any prior event, so it is causally
/// faithful regardless of sub-second timestamp ties. Exercises the whole
/// primitive: merge → derive → surface → wake the *peer* (F8, never the author —
/// F5) → react → merge back. Asserts every move was driven by a real wake (the
/// loop completes) and both agents converge to the byte-identical document.
async fn ping_pong_for(channel: Channel) {
    // Total moves across both agents (each wake must fire for the loop to finish).
    const MOVES: usize = 6;

    let mut alice = InProcNode::create(&mesh_name(channel, "ss-pp")).await;
    let mut bob = InProcNode::join(&alice.mesh, "pp-bob").await;

    alice.send("link").await;
    assert!(bob.wait_body("link", MSG_TIMEOUT).await, "bob meshed");

    // alice opens with move 1 under its own key — no container to seed first.
    alice.merge(channel, json!({"m1": "alice"})).await;

    // Alternate: the mover for move `target` is bob when even, alice when odd.
    // Each waits until it sees the peer's previous move land (the document then
    // holds `target - 1` moves) — a real wake — then records its own.
    for target in 2..=MOVES {
        let author = if target % 2 == 0 { "bob" } else { "alice" };
        let merge = one(format!("m{target}"), json!(author));
        if target % 2 == 0 {
            assert!(
                bob.wait_change(channel, MSG_TIMEOUT, |doc| move_count(doc) == target - 1)
                    .await,
                "bob not woken to make move {target} on {}",
                label(channel)
            );
            bob.merge(channel, merge).await;
        } else {
            assert!(
                alice
                    .wait_change(channel, MSG_TIMEOUT, |doc| move_count(doc) == target - 1)
                    .await,
                "alice not woken to make move {target} on {}",
                label(channel)
            );
            alice.merge(channel, merge).await;
        }
    }

    // Both converge to the same document of exactly `MOVES` moves (no lost or
    // doubled move — the wake-driven alternation produced one move per step).
    let converged = |doc: &Value| move_count(doc) == MOVES;
    assert!(
        wait_doc(&alice, channel, RECOVERY_TIMEOUT, converged).await,
        "alice did not converge to {MOVES} moves on {}",
        label(channel)
    );
    assert!(
        wait_doc(&bob, channel, RECOVERY_TIMEOUT, converged).await,
        "bob did not converge to {MOVES} moves on {}",
        label(channel)
    );
    assert_eq!(
        app_view(&alice.get(channel).await),
        app_view(&bob.get(channel).await),
        "both agents must derive the byte-identical {} document",
        label(channel)
    );

    alice.leave().await;
    bob.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn state_two_agents_ping_pong_via_shared_state() {
    ping_pong_for(Channel::State).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn meta_two_agents_ping_pong_via_shared_state() {
    ping_pong_for(Channel::Meta).await;
}

/// A member may only write its OWN card: a signed meta merge forging another
/// peer's `/peers/<nick>/card` (here, a fake gossip-interface identity url) is
/// dropped by every recipient before it folds, so the victim's genuine card —
/// and the identity in it — survives. Without the gate the forgery would win the
/// last-writer-wins fold and spoof the victim to `agent-gossip card` and `--a2a-serve`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreign_card_forgery_is_rejected() {
    let alice = InProcNode::create("ss-forge").await;
    let mut bob = InProcNode::join(&alice.mesh, "forge-bob").await;
    alice.send("link").await;
    assert!(bob.wait_body("link", MSG_TIMEOUT).await, "bob meshed");

    // Both cards publish on join; wait until alice sees bob's genuine card.
    let bob_nick = bob.nickname.clone();
    let identity_ptr = format!("/peers/{bob_nick}/card/supportedInterfaces/0/url");
    assert!(
        wait_doc(&alice, Channel::Meta, RECOVERY_TIMEOUT, |doc| doc
            .pointer(&identity_ptr)
            .is_some())
        .await,
        "alice never saw bob's genuine card"
    );
    let genuine_url = alice
        .meta_get()
        .await
        .pointer(&identity_ptr)
        .and_then(Value::as_str)
        .expect("bob's genuine gossip-interface url")
        .to_owned();

    // Alice tries to forge bob's card with a fake identity. The card-forgery
    // gate runs on the author's own doc too, so the write is refused at the
    // source (an honest daemon cannot author it) — the receive-side gate against
    // a *malicious* peer that bypasses this path is exercised by the adversarial
    // suite injecting crafted frames. A legit self-write is the delivery barrier.
    let fake_url = format!("🤖://{}", "ff".repeat(32));
    let forged = alice
        .try_meta_merge(
            json!({"peers": {&bob_nick: {"card": {"supportedInterfaces": [
                {"url": fake_url, "protocolBinding": "x", "protocolVersion": "1.0"}
            ]}}}}),
        )
        .await;
    assert!(
        forged.is_err(),
        "forging another peer's card must be rejected at the source"
    );
    alice
        .meta_merge(json!({"peers": {alice.nickname.as_str(): {"note": "barrier"}}}))
        .await;
    assert!(
        wait_doc(&bob, Channel::Meta, RECOVERY_TIMEOUT, |doc| doc
            .pointer(&format!("/peers/{}/note", alice.nickname))
            == Some(&json!("barrier")))
        .await,
        "barrier merge never derived on bob"
    );

    // Bob's view of his OWN card must still carry his genuine identity — the
    // forgery never entered any doc.
    assert_eq!(
        bob.meta_get()
            .await
            .pointer(&identity_ptr)
            .and_then(Value::as_str),
        Some(genuine_url.as_str()),
        "a forged foreign card must never overwrite the victim's genuine card"
    );

    alice.leave().await;
    bob.leave().await;
}

/// Every member's daemon publishes its `AgentCard` at meta `/peers/<nick>/card`
/// on join — the mesh-native discovery path (no HTTP anywhere). Both sides
/// derive each other's card from the meta document, and the card carries the
/// A2A protocol version, the declared mesh extensions, and the member's
/// Ed25519 identity.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agent_cards_publish_to_meta_on_join() {
    let alice = InProcNode::create("ss-cards").await;
    let mut bob = InProcNode::join(&alice.mesh, "cards-bob").await;
    alice.send("link").await;
    assert!(bob.wait_body("link", MSG_TIMEOUT).await, "bob meshed");

    let has_card = |doc: &Value, nick: &str| {
        let card = doc.pointer(&format!("/peers/{nick}/card"));
        card.is_some_and(|card| {
            card["name"] == *nick
                && card["capabilities"]["extensions"]
                    .as_array()
                    .is_some_and(|exts| !exts.is_empty())
                // v1.0: the protocol version + the Ed25519 identity ride the
                // gossip `AgentInterface` (`🤖://<pubkey>`).
                && card["supportedInterfaces"][0]["protocolVersion"].is_string()
                && card["supportedInterfaces"][0]["url"]
                    .as_str()
                    .and_then(|url| url.strip_prefix("🤖://"))
                    .is_some_and(|key| key.len() == 64)
                // The stable dial hint (EndpointId + home relay) rides inside the
                // card, so a synced peer can dial without a gossiped PeerInfo.
                && card["endpoint"]["id"].is_string()
        })
    };

    let alice_nick = alice.nickname.clone();
    assert!(
        wait_doc(&bob, Channel::Meta, RECOVERY_TIMEOUT, |doc| has_card(
            doc,
            &alice_nick
        ))
        .await,
        "bob never derived alice's card: {}",
        bob.meta_get().await
    );
    assert!(
        wait_doc(&alice, Channel::Meta, RECOVERY_TIMEOUT, |doc| has_card(
            doc,
            "cards-bob"
        ))
        .await,
        "alice never derived bob's card: {}",
        alice.meta_get().await
    );

    alice.leave().await;
    bob.leave().await;
}
