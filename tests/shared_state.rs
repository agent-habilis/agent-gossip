//! Shared-state behavioral tests, in-process over the real event loop and a real
//! iroh mesh (`common::InProcNode`). They exercise the whole primitive — apply a
//! JSON-Patch → fold the log → derive the document → surface the change → react →
//! patch back — with no app logic in the daemon.
//!
//! Every behavior runs against **both channels** (`state` and `meta`): each test
//! body is `*_for(channel)`, with thin per-channel `#[tokio::test]` wrappers
//! (`state_*` / `meta_*`). The two channels share one code path parameterized by
//! `Channel`, so this proves parity by construction — a regression on either
//! channel turns exactly one named test red.
//!
//! What each behavior pins:
//! - convergence: the fold is a deterministic function of the event *set*;
//! - boundary validation + atomicity (F7): a bad/partial patch never mutates;
//! - the reaction hook (F8) and the self-wake guard (F5): a peer's change wakes
//!   an agent with the derived document, its own change does not;
//! - the shared rate limit (F2): a patch flood is throttled to the quota;
//! - the unbounded log + windowed anti-entropy: a late joiner reconciles a log
//!   far larger than one digest window;
//! - reaction + convergence end-to-end: two agents ping-pong a counter via the
//!   shared document and both converge, strictly alternating (no double-move);
//! - compare-and-set (`--if-doc-hash`): a stale-guarded patch is rejected.
//!
//! `meta_and_state_channels_are_independent` is the one inherently cross-channel
//! test and stays standalone.

mod common;

use std::time::{Duration, Instant};

use agent_habilis_swarm::Channel;
use common::{InProcNode, MSG_TIMEOUT, POLL, RECOVERY_TIMEOUT};
use serde_json::{Value, json};

/// `state` / `meta`, for naming swarms and assert messages (the crate's
/// `Channel::label` is `pub(crate)`, not reachable from this test crate).
fn label(channel: Channel) -> &'static str {
    match channel {
        Channel::State => "state",
        Channel::Meta => "meta",
    }
}

/// A per-channel swarm name so the `state` and `meta` variants of one behavior
/// never share a name when they run concurrently.
fn swarm_name(channel: Channel, base: &str) -> String {
    format!("{base}-{}", label(channel))
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
    let mut bob = InProcNode::join(&alice.swarm, "ch-bob").await;
    alice.send("link").await;
    assert!(bob.wait_body("link", MSG_TIMEOUT).await, "bob meshed");

    // A meta write propagates to bob's meta doc; both state docs stay empty.
    alice
        .meta_patch(json!([{"op":"add","path":"/peers","value":{"alice":{"model":"Opus 4.8"}}}]))
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
    alice
        .state_patch(json!([{"op":"add","path":"/turn","value":"a"}]))
        .await;
    assert!(
        wait_doc(&bob, Channel::State, RECOVERY_TIMEOUT, |doc| doc
            .pointer("/turn")
            == Some(&json!("a")))
        .await,
        "bob never saw the state write: {}",
        bob.state_get().await
    );
    // meta still holds only the earlier write.
    assert_eq!(
        alice.meta_get().await,
        json!({"peers": {"alice": {"model": "Opus 4.8"}}}),
        "state write must not touch meta"
    );

    alice.leave().await;
    bob.leave().await;
}

/// Three peers each apply an independent patch; whatever order the patches
/// arrive in, every peer folds the same event *set* into the byte-identical
/// document. Proves the reducer is set-deterministic (the convergence property).
async fn patches_converge_for(channel: Channel) {
    let alice = InProcNode::create(&swarm_name(channel, "ss-conv")).await;
    let mut bob = InProcNode::join(&alice.swarm, "conv-bob").await;
    let mut carol = InProcNode::join(&alice.swarm, "conv-carol").await;

    // Mesh first (a delivered message proves the links).
    alice.send("link").await;
    assert!(bob.wait_body("link", MSG_TIMEOUT).await, "bob meshed");
    assert!(carol.wait_body("link", MSG_TIMEOUT).await, "carol meshed");

    // Each peer seeds a distinct key concurrently — intermediate states differ
    // by arrival order, the final set does not.
    alice
        .patch(channel, json!([{"op": "add", "path": "/a", "value": 1}]))
        .await;
    bob.patch(channel, json!([{"op": "add", "path": "/b", "value": 2}]))
        .await;
    carol
        .patch(channel, json!([{"op": "add", "path": "/c", "value": 3}]))
        .await;

    let want = json!({"a": 1, "b": 2, "c": 3});
    for (node, who) in [(&alice, "alice"), (&bob, "bob"), (&carol, "carol")] {
        assert!(
            wait_doc(node, channel, RECOVERY_TIMEOUT, |doc| doc == &want).await,
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

/// A malformed, out-of-subset, non-applying, or partially-applying patch is
/// rejected at the apply boundary and never mutates the document (F7: the apply
/// is atomic — a clone is committed only if every op succeeds).
async fn invalid_patches_rejected_for(channel: Channel) {
    let alice = InProcNode::create(&swarm_name(channel, "ss-reject")).await;
    alice
        .patch(channel, json!([{"op": "add", "path": "/n", "value": 1}]))
        .await;

    // Out-of-subset op (`move`): rejected.
    assert!(
        alice
            .try_patch(channel, json!([{"op": "move", "from": "/n", "path": "/m"}]))
            .await
            .is_err(),
        "move is outside the frozen subset and must be rejected"
    );
    // Non-applying op (`replace` a missing path): rejected.
    assert!(
        alice
            .try_patch(
                channel,
                json!([{"op": "replace", "path": "/missing", "value": 9}])
            )
            .await
            .is_err(),
        "replace on a missing path does not apply and must be rejected"
    );
    // Atomicity: a two-op patch whose second op fails must leave NO trace of the
    // first (no partial `/ok`).
    assert!(
        alice
            .try_patch(
                channel,
                json!([
                    {"op": "add", "path": "/ok", "value": 1},
                    {"op": "replace", "path": "/missing", "value": 9}
                ])
            )
            .await
            .is_err(),
        "a partially-applying patch must be rejected whole"
    );

    assert_eq!(
        alice.get(channel).await,
        json!({"n": 1}),
        "no rejected patch may mutate the {} document",
        label(channel)
    );
    alice.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_invalid_patches_are_rejected_and_leave_the_document_untouched() {
    invalid_patches_rejected_for(Channel::State).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn meta_invalid_patches_are_rejected_and_leave_the_document_untouched() {
    invalid_patches_rejected_for(Channel::Meta).await;
}

/// The reaction hook (F8) and the self-wake guard (F5): a peer's patch wakes an
/// agent on its `events()` channel carrying the freshly-derived document, while
/// the author is **not** woken on its own channel for its own patch.
async fn peer_change_wakes_for(channel: Channel) {
    let mut alice = InProcNode::create(&swarm_name(channel, "ss-wake")).await;
    let mut bob = InProcNode::join(&alice.swarm, "wake-bob").await;

    alice.send("link").await;
    assert!(bob.wait_body("link", MSG_TIMEOUT).await, "bob meshed");

    alice
        .patch(
            channel,
            json!([{"op": "add", "path": "/turn", "value": "bob"}]),
        )
        .await;

    // F8: bob is woken with the document.
    assert!(
        bob.wait_change(channel, MSG_TIMEOUT, |doc| doc["turn"] == "bob")
            .await,
        "a peer's {} change must wake the agent with the derived document (F8)",
        label(channel)
    );
    // F5: alice saw no change on her own wake channel.
    assert!(
        alice.changes(channel).is_empty(),
        "an agent must not be woken on its own {} patch (F5), got {:?}",
        label(channel),
        alice.changes(channel)
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

/// The shared rate limit (F2): a burst of patches beyond the per-author quota is
/// throttled — the sender-side limiter drops the excess rather than broadcasting
/// it, identical to the chat-message quota. (The limiter is shared across
/// channels; a fresh node per test keeps the channels' floods isolated.)
async fn patch_flood_rate_limited_for(channel: Channel) {
    // A small quota so the flood trips it deterministically and fast.
    let alice = InProcNode::create_rate_limited(&swarm_name(channel, "ss-rl"), 5).await;

    let mut accepted = 0u32;
    let mut limited = 0u32;
    for index in 0..20 {
        match alice
            .try_patch(
                channel,
                json!([{"op": "add", "path": format!("/k{index}"), "value": index}]),
            )
            .await
        {
            Ok(()) => accepted += 1,
            Err(error) if error.to_string().contains("rate limited") => limited += 1,
            Err(error) => panic!("unexpected apply error on {}: {error}", label(channel)),
        }
    }

    assert!(
        (1..=8).contains(&accepted),
        "about the quota of {} patches should be accepted, got {accepted}",
        label(channel)
    );
    assert!(
        limited >= 1,
        "{} patches beyond the quota must be rate-limited, got {limited}",
        label(channel)
    );
    alice.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_patch_flood_is_rate_limited() {
    patch_flood_rate_limited_for(Channel::State).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn meta_patch_flood_is_rate_limited() {
    patch_flood_rate_limited_for(Channel::Meta).await;
}

/// Unbounded log + windowed anti-entropy: drive far more patches than one digest
/// window holds, then a late joiner — arriving after all traffic, so only
/// anti-entropy can reach it — reconciles the *whole* log across several windows
/// and derives the identical document.
///
/// Each patch sets an independent key (`/k0`, `/k1`, …), so the *set* of events
/// fully determines the document regardless of replay order — the property under
/// test is completeness of backfill, not causal ordering.
async fn late_joiner_backfills_for(channel: Channel) {
    // Comfortably more than two digest windows (`ANTIENTROPY_DIGEST_WINDOW_IDS`
    // is 70), so the rolling older-window cursor is genuinely exercised.
    const PATCHES: usize = 160;

    let alice = InProcNode::create_unlimited(&swarm_name(channel, "ss-backfill")).await;
    let early = InProcNode::join(&alice.swarm, "bf-early").await;
    // Mesh so the appends go out live (the creator's outbound buffer stays
    // empty) — the late joiner can then only be served by anti-entropy.
    alice.send("link").await;

    for index in 0..PATCHES {
        alice
            .patch(
                channel,
                json!([{"op": "add", "path": format!("/k{index}"), "value": index}]),
            )
            .await;
    }

    let want = alice.get(channel).await;
    assert_eq!(
        want.as_object().map(serde_json::Map::len),
        Some(PATCHES),
        "creator's own {} document holds every key it set",
        label(channel)
    );

    // The early (meshed) peer converges via the live path.
    assert!(
        wait_doc(&early, channel, RECOVERY_TIMEOUT, |doc| doc == &want).await,
        "early peer never converged on the live {} path",
        label(channel)
    );

    // The late joiner arrives after all traffic — windowed anti-entropy, across
    // multiple rounds, must reconstruct the full log.
    let late = InProcNode::join(&alice.swarm, "bf-late").await;
    assert!(
        wait_doc(&late, channel, Duration::from_secs(150), |doc| doc == &want).await,
        "late joiner never backfilled the full {} log via windowed anti-entropy ({} of {PATCHES} keys)",
        label(channel),
        late.get(channel)
            .await
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

/// The number of moves recorded in the shared document (one `/m{k}` key each).
fn move_count(doc: &Value) -> usize {
    doc.as_object().map_or(0, serde_json::Map::len)
}

/// Reaction + convergence end-to-end (the chess narrative, no engine): two
/// agents ping-pong, each reacting to the other's change before making its own.
/// Every move records itself under its OWN key (`/m1`, `/m2`, …) — an
/// independent `add`, with no dependency on any prior event, so it is causally
/// faithful regardless of sub-second timestamp ties. Exercises the whole
/// primitive: patch → derive → surface → wake the *peer* (F8, never the author —
/// F5) → react → patch back. Asserts every move was driven by a real wake (the
/// loop completes) and both agents converge to the byte-identical document.
async fn ping_pong_for(channel: Channel) {
    // Total moves across both agents (each wake must fire for the loop to finish).
    const MOVES: usize = 6;

    let mut alice = InProcNode::create(&swarm_name(channel, "ss-pp")).await;
    let mut bob = InProcNode::join(&alice.swarm, "pp-bob").await;

    alice.send("link").await;
    assert!(bob.wait_body("link", MSG_TIMEOUT).await, "bob meshed");

    // alice opens with move 1 under its own key — no container to seed first.
    alice
        .patch(
            channel,
            json!([{"op": "add", "path": "/m1", "value": "alice"}]),
        )
        .await;

    // Alternate: the mover for move `target` is bob when even, alice when odd.
    // Each waits until it sees the peer's previous move land (the document then
    // holds `target - 1` moves) — a real wake — then records its own.
    for target in 2..=MOVES {
        let author = if target % 2 == 0 { "bob" } else { "alice" };
        let patch = json!([{"op": "add", "path": format!("/m{target}"), "value": author}]);
        if target % 2 == 0 {
            assert!(
                bob.wait_change(channel, MSG_TIMEOUT, |doc| move_count(doc) == target - 1)
                    .await,
                "bob not woken to make move {target} on {}",
                label(channel)
            );
            bob.patch(channel, patch).await;
        } else {
            assert!(
                alice
                    .wait_change(channel, MSG_TIMEOUT, |doc| move_count(doc) == target - 1)
                    .await,
                "alice not woken to make move {target} on {}",
                label(channel)
            );
            alice.patch(channel, patch).await;
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
        alice.get(channel).await,
        bob.get(channel).await,
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

/// Compare-and-set (`--if-doc-hash`): a patch guarded by a stale document hash
/// is rejected without mutating; the same patch with the current hash applies.
/// This is the optimistic-concurrency guard that stops a concurrent peer's
/// change from being silently clobbered. The guard is local to the author, so a
/// single node exercises it fully.
async fn cas_for(channel: Channel) {
    let node = InProcNode::create(&swarm_name(channel, "ss-cas")).await;
    // Each patch adds a *distinct* key, so the fold is order-independent — the
    // test exercises the CAS guard, not the `(timestamp, id)` tie-break that a
    // same-key replace within one second would expose.
    node.patch(
        channel,
        json!([{"op": "add", "path": "/seed", "value": "x"}]),
    )
    .await;

    let before = node.get(channel).await;
    let hash = agent_habilis_swarm::document_hash(&before);

    // A guard hash that doesn't match the current document is rejected, and the
    // document is left unchanged.
    let stale = node
        .try_patch_if(
            channel,
            json!([{"op": "add", "path": "/a", "value": 1}]),
            Some("deadbeef".to_owned()),
        )
        .await;
    assert!(
        stale.is_err(),
        "stale-hash {} patch must be rejected",
        label(channel)
    );
    assert!(
        stale.unwrap_err().to_string().contains("stale document"),
        "rejection names the cause"
    );
    assert_eq!(
        node.get(channel).await,
        before,
        "a rejected CAS patch must not mutate the {} document",
        label(channel)
    );

    // The same patch with the *current* hash applies.
    node.try_patch_if(
        channel,
        json!([{"op": "add", "path": "/a", "value": 1}]),
        Some(hash.clone()),
    )
    .await
    .expect("current-hash patch applies");
    assert_eq!(node.get(channel).await["a"], json!(1));

    // The now-superseded hash is stale and re-rejected.
    let after = node
        .try_patch_if(
            channel,
            json!([{"op": "add", "path": "/b", "value": 2}]),
            Some(hash),
        )
        .await;
    assert!(
        after.is_err(),
        "a superseded {} hash must be rejected",
        label(channel)
    );

    node.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_cas_rejects_stale_hash_and_accepts_current() {
    cas_for(Channel::State).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn meta_cas_rejects_stale_hash_and_accepts_current() {
    cas_for(Channel::Meta).await;
}
