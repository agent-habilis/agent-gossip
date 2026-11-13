//! Shared-state behavioral tests, in-process over the real event loop and a real
//! iroh mesh (`common::InProcNode`). They exercise the whole primitive — write the
//! own slice → gossip it → derive the view → surface the change → react → write
//! back — with no app logic in the daemon.
//!
//! The view of a channel holds one signed slice for each author, keyed by the hex of
//! the author key. The tests write disjoint keys, so `merged` folds the slices into
//! one document that a test can compare.
//!
//! Every behavior runs against **both channels** (`state` and `meta`): each test
//! body is `*_for(channel)`, with thin per-channel `#[tokio::test]` wrappers
//! (`state_*` / `meta_*`). The two channels share one code path parameterized by
//! `Channel`, so this proves parity by construction — a regression on either
//! channel turns exactly one named test red.
//!
//! What each behavior pins:
//! - convergence: every member derives the same set of slices;
//! - the reaction hook (F8) and the self-wake guard (F5): a peer's change wakes
//!   an agent with the derived view, its own change does not;
//! - the late joiner: a member that arrives after all traffic gets the current slice of
//!   every author;
//! - reaction + convergence end-to-end: two agents ping-pong a counter via the
//!   shared document and both converge, strictly alternating (no double-move);
//! - a graceful leave removes the slice of the leaver, and a rejoin writes a new one.
//!
//! `meta_and_state_channels_are_independent` is the one inherently cross-channel
//! test and stays standalone.

use std::time::{Duration, Instant};

use agent_gossip::Channel;
use agent_gossip_test_fixtures as common;
use common::{InProcNode, MSG_TIMEOUT, POLL, RECOVERY_TIMEOUT};
use serde_json::{Value, json};

/// `state` / `meta`, for naming meshes and assert messages (the crate's
/// `Channel::label` is `pub(crate)`, not reachable from this test crate).
fn label(channel: Channel) -> &'static str {
    match channel {
        Channel::State => "state",
        Channel::Meta => "meta",
        _ => panic!("the test has no label for this channel"),
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

/// The slices of a channel view folded into one document. The tests write disjoint keys, so the
/// union of the keys of all slices is the document that the members agree on.
fn merged(view: &Value) -> Value {
    let mut document = serde_json::Map::new();
    for slice in view
        .as_object()
        .into_iter()
        .flat_map(serde_json::Map::values)
    {
        for (key, value) in slice.as_object().into_iter().flatten() {
            document.insert(key.clone(), value.clone());
        }
    }
    Value::Object(document)
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
    alice.broadcast("link").await;
    assert!(bob.wait_body("link", MSG_TIMEOUT).await, "bob meshed");

    // A meta write propagates to bob's meta view; both state views stay empty.
    // The slot of a member is the hex of its author key.
    let alice_slot = alice.key_seen_by(&bob).await.to_string();
    alice.meta_merge(json!({"model": "Opus 4.8"})).await;
    assert!(
        wait_doc(&bob, Channel::Meta, RECOVERY_TIMEOUT, |doc| {
            doc.pointer(&format!("/{alice_slot}/model")) == Some(&json!("Opus 4.8"))
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
        wait_doc(
            &bob,
            Channel::State,
            RECOVERY_TIMEOUT,
            |doc| merged(doc)["turn"] == json!("a")
        )
        .await,
        "bob never saw the state write: {}",
        bob.state_get().await
    );
    // meta still holds only the earlier write.
    assert_eq!(
        alice.meta_get().await,
        json!({ alice_slot.as_str(): {"model": "Opus 4.8"} }),
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
    alice.broadcast("link").await;
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
            wait_doc(node, channel, RECOVERY_TIMEOUT, |doc| merged(doc) == want).await,
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

    alice.broadcast("link").await;
    assert!(bob.wait_body("link", MSG_TIMEOUT).await, "bob meshed");

    alice.merge(channel, json!({"turn": "bob"})).await;

    // F8: bob is woken with the document.
    assert!(
        bob.wait_change(channel, MSG_TIMEOUT, |doc| merged(doc)["turn"] == "bob")
            .await,
        "a peer's {} change must wake the agent with the derived document (F8)",
        label(channel)
    );
    // F5: alice was never woken by her OWN merge (a peer's change wakes her
    // with `self:false`, so filter on the flag).
    let self_wakes: Vec<_> = alice
        .changes(channel)
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

/// A late joiner — arriving after all traffic — gets the current slice of every author and derives
/// the same document as the others. The engine keeps one slice for each author, so there is no log
/// to replay: the property under test is that the latest slice reaches a member that was not there.
async fn late_joiner_gets_the_slices_for(channel: Channel) {
    // Few writes: an author has a burst and a rate for new versions.
    const WRITES: usize = 5;
    const KEYS_PER_WRITE: usize = 20;

    let alice = InProcNode::create(&mesh_name(channel, "ss-backfill")).await;
    let early = InProcNode::join(&alice.mesh, "bf-early").await;
    alice.broadcast("link").await;

    for write in 0..WRITES {
        let mut keys = serde_json::Map::new();
        for key in 0..KEYS_PER_WRITE {
            let index = write * KEYS_PER_WRITE + key;
            keys.insert(format!("k{index}"), json!(index));
        }
        alice.merge(channel, Value::Object(keys)).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    let want = merged(&alice.get(channel).await);
    assert_eq!(
        want.as_object().map(serde_json::Map::len),
        Some(WRITES * KEYS_PER_WRITE),
        "creator's own {} view holds every key it set",
        label(channel)
    );

    // The early (meshed) peer converges via the live path.
    assert!(
        wait_doc(&early, channel, RECOVERY_TIMEOUT, |doc| merged(doc) == want).await,
        "early peer never converged on the live {} path",
        label(channel)
    );

    // The late joiner arrives after all traffic and gets the latest slice of the creator.
    let late = InProcNode::join(&alice.mesh, "bf-late").await;
    assert!(
        wait_doc(&late, channel, Duration::from_secs(150), |doc| merged(doc)
            == want)
        .await,
        "late joiner never got the {} slices ({} of {} keys)",
        label(channel),
        merged(&late.get(channel).await)
            .as_object()
            .map_or(0, serde_json::Map::len),
        WRITES * KEYS_PER_WRITE
    );

    alice.leave().await;
    early.leave().await;
    late.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn state_late_joiner_gets_the_slice_of_every_member() {
    late_joiner_gets_the_slices_for(Channel::State).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn meta_late_joiner_gets_the_slice_of_every_member() {
    late_joiner_gets_the_slices_for(Channel::Meta).await;
}

/// The number of moves recorded in the shared document (one `/m{k}` key
/// each).
fn move_count(doc: &Value) -> usize {
    merged(doc).as_object().map_or(0, serde_json::Map::len)
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

    alice.broadcast("link").await;
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
        merged(&alice.get(channel).await),
        merged(&bob.get(channel).await),
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

/// A graceful leave removes the whole slice of the leaver on the remaining replicas, and a later
/// joiner gets the view without it. The leaver is a joiner, not the creator: a departed creator
/// also takes the rendezvous beacon with it for ~36s, which is the handoff's own test.
///
/// The engine tombstones the slice on a graceful leave. The test still proves the leaver→host path
/// live right before the leave, so a failure indicts the removal, not the warmup.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn graceful_leave_removes_the_slice_of_the_leaver() {
    let mut host = InProcNode::create_with_nick("ss-retract", "retract-host").await;
    let mut leaver = InProcNode::join(&host.mesh, "retract-leaver").await;
    host.broadcast("link").await;
    assert!(leaver.wait_body("link", MSG_TIMEOUT).await, "leaver meshed");

    // The slot of a member is the hex of its author key.
    let leaver_slot = leaver.key_seen_by(&host).await.to_string();
    let host_slot = host.key_seen_by(&leaver).await.to_string();

    host.meta_merge(json!({"model": "host-model"})).await;
    leaver.meta_merge(json!({"model": "test-model"})).await;
    assert!(
        wait_doc(&host, Channel::Meta, RECOVERY_TIMEOUT, |doc| {
            doc.pointer(&format!("/{leaver_slot}/model")) == Some(&json!("test-model"))
        })
        .await,
        "the host never derived the slice of the leaver: {}",
        host.meta_get().await
    );

    // Prove the leaver→host overlay path is live *now*, so the farewell
    // frames that follow ride a link that demonstrably works.
    leaver.broadcast("goodbye-imminent").await;
    assert!(
        host.wait_body("goodbye-imminent", MSG_TIMEOUT).await,
        "leaver→host path was not live before the leave"
    );

    leaver.leave().await;

    assert!(
        wait_doc(&host, Channel::Meta, RECOVERY_TIMEOUT, |doc| {
            doc.pointer(&format!("/{leaver_slot}")).is_none()
        })
        .await,
        "the slice of the leaver survived its graceful leave: {}",
        host.meta_get().await
    );

    // A late joiner gets the slice of the host, and none of the leaver. The warmup send is
    // retried: a frame sent into the post-join overlay flap is dropped for good.
    let mut late = InProcNode::join(&host.mesh, "retract-late").await;
    let mut meshed = false;
    for attempt in 0..3 {
        host.broadcast(&format!("relink-{attempt}")).await;
        if late
            .wait_body(&format!("relink-{attempt}"), MSG_TIMEOUT)
            .await
        {
            meshed = true;
            break;
        }
    }
    assert!(meshed, "late joiner never meshed");
    assert!(
        wait_doc(&late, Channel::Meta, RECOVERY_TIMEOUT, |doc| {
            doc.pointer(&format!("/{host_slot}/model")).is_some()
                && doc.pointer(&format!("/{leaver_slot}")).is_none()
        })
        .await,
        "the late joiner got the slice of the leaver, or none of the host: {}",
        late.meta_get().await
    );

    host.leave().await;
    late.leave().await;
}

/// Rejoining under the same nickname after a graceful leave writes a new slice: the new run has a
/// new key, so its slice is a real addition on top of the removal.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn meta_entry_returns_after_rejoin() {
    let mut host = InProcNode::create_with_nick("ss-rejoin", "rejoin-host").await;
    let mut leaver = InProcNode::join(&host.mesh, "rejoin-peer").await;
    host.broadcast("link").await;
    assert!(leaver.wait_body("link", MSG_TIMEOUT).await, "peer meshed");

    // The slot of a member is the hex of its author key.
    let first_slot = leaver.key_seen_by(&host).await.to_string();
    leaver.meta_merge(json!({"nickname": "rejoin-peer"})).await;
    assert!(
        wait_doc(&host, Channel::Meta, RECOVERY_TIMEOUT, |doc| {
            doc.pointer(&format!("/{first_slot}/nickname")).is_some()
        })
        .await,
        "the host never derived the slice of the peer"
    );
    // Same live-path proof as `graceful_leave_removes_the_slice_of_the_leaver`.
    leaver.broadcast("goodbye-imminent").await;
    assert!(
        host.wait_body("goodbye-imminent", MSG_TIMEOUT).await,
        "leaver→host path was not live before the leave"
    );
    leaver.leave().await;
    assert!(
        wait_doc(&host, Channel::Meta, RECOVERY_TIMEOUT, |doc| {
            doc.pointer(&format!("/{first_slot}")).is_none()
        })
        .await,
        "the removal never reached the host: {}",
        host.meta_get().await
    );

    // Same nickname, fresh identity — the remove-then-add case. The new key has a new slot, so
    // the slice to wait for is the one under it. The rejoin can land while the host's gossip still
    // holds the dead session's stale link, so the joiner may only integrate on the next heal tick
    // — retry the warmup instead of trusting the first frame.
    let mut returned = InProcNode::join(&host.mesh, "rejoin-peer").await;
    let mut meshed = false;
    for attempt in 0..3 {
        host.broadcast(&format!("welcome-back-{attempt}")).await;
        if returned
            .wait_body(&format!("welcome-back-{attempt}"), MSG_TIMEOUT)
            .await
        {
            meshed = true;
            break;
        }
    }
    assert!(meshed, "the returned peer never meshed");
    returned
        .meta_merge(json!({"nickname": "rejoin-peer"}))
        .await;
    assert!(
        wait_doc(&host, Channel::Meta, RECOVERY_TIMEOUT, |doc| {
            common::card_slot_of(doc, "rejoin-peer").is_some_and(|slot| slot != first_slot)
        })
        .await,
        "the rejoined peer's slice never appeared: {}",
        host.meta_get().await
    );

    returned.leave().await;
    host.leave().await;
}
