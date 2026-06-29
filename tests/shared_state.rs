//! Shared-state (Phase 1) behavioral tests, in-process over the real event loop
//! and a real iroh mesh (`common::InProcNode`). They exercise the whole
//! primitive — apply a JSON-Patch → fold the log → derive the document →
//! surface the change → react → patch back — with no app logic in the daemon.
//!
//! What each test pins:
//! - convergence: the fold is a deterministic function of the event *set*;
//! - boundary validation + atomicity (F7): a bad/partial patch never mutates;
//! - the reaction hook (F8) and the self-wake guard (F5): a peer's change wakes
//!   an agent with the derived document, its own change does not;
//! - the unbounded log + windowed anti-entropy: a late joiner reconciles a log
//!   far larger than one digest window;
//! - reaction + convergence end-to-end: two agents ping-pong a counter via the
//!   shared document and both converge, strictly alternating (no double-move).

mod common;

use std::time::{Duration, Instant};

use common::{InProcNode, MSG_TIMEOUT, POLL, RECOVERY_TIMEOUT};
use serde_json::{Value, json};

/// Poll a node's derived document until `pred` holds or `timeout` elapses.
async fn wait_doc(
    node: &InProcNode,
    timeout: Duration,
    mut pred: impl FnMut(&Value) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if pred(&node.state_document().await) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Three peers each apply an independent patch; whatever order the patches
/// arrive in, every peer folds the same event *set* into the byte-identical
/// document. Proves the reducer is set-deterministic (the convergence property).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn patches_converge_to_identical_documents() {
    let alice = InProcNode::create("ss-conv").await;
    let mut bob = InProcNode::join(&alice.swarm, "conv-bob").await;
    let mut carol = InProcNode::join(&alice.swarm, "conv-carol").await;

    // Mesh first (a delivered message proves the links).
    alice.send("link").await;
    assert!(bob.wait_body("link", MSG_TIMEOUT).await, "bob meshed");
    assert!(carol.wait_body("link", MSG_TIMEOUT).await, "carol meshed");

    // Each peer seeds a distinct key concurrently — intermediate states differ
    // by arrival order, the final set does not.
    alice
        .apply_patch(json!([{"op": "add", "path": "/a", "value": 1}]))
        .await;
    bob.apply_patch(json!([{"op": "add", "path": "/b", "value": 2}]))
        .await;
    carol
        .apply_patch(json!([{"op": "add", "path": "/c", "value": 3}]))
        .await;

    let want = json!({"a": 1, "b": 2, "c": 3});
    for (node, who) in [(&alice, "alice"), (&bob, "bob"), (&carol, "carol")] {
        assert!(
            wait_doc(node, RECOVERY_TIMEOUT, |doc| doc == &want).await,
            "{who} never converged: {}",
            node.state_document().await
        );
    }

    alice.leave().await;
    bob.leave().await;
    carol.leave().await;
}

/// A malformed, out-of-subset, non-applying, or partially-applying patch is
/// rejected at the apply boundary and never mutates the document (F7: the apply
/// is atomic — a clone is committed only if every op succeeds).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_patches_are_rejected_and_leave_the_document_untouched() {
    let alice = InProcNode::create("ss-reject").await;
    alice
        .apply_patch(json!([{"op": "add", "path": "/n", "value": 1}]))
        .await;

    // Out-of-subset op (`move`): rejected.
    assert!(
        alice
            .try_apply_patch(json!([{"op": "move", "from": "/n", "path": "/m"}]))
            .await
            .is_err(),
        "move is outside the frozen subset and must be rejected"
    );
    // Non-applying op (`replace` a missing path): rejected.
    assert!(
        alice
            .try_apply_patch(json!([{"op": "replace", "path": "/missing", "value": 9}]))
            .await
            .is_err(),
        "replace on a missing path does not apply and must be rejected"
    );
    // Atomicity: a two-op patch whose second op fails must leave NO trace of the
    // first (no partial `/ok`).
    assert!(
        alice
            .try_apply_patch(json!([
                {"op": "add", "path": "/ok", "value": 1},
                {"op": "replace", "path": "/missing", "value": 9}
            ]))
            .await
            .is_err(),
        "a partially-applying patch must be rejected whole"
    );

    assert_eq!(
        alice.state_document().await,
        json!({"n": 1}),
        "no rejected patch may mutate the document"
    );
    alice.leave().await;
}

/// The reaction hook (F8) and the self-wake guard (F5): a peer's patch wakes an
/// agent on its `events()` channel carrying the freshly-derived document, while
/// the author is **not** woken on its own channel for its own patch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn peer_change_wakes_the_agent_self_change_does_not() {
    let mut alice = InProcNode::create("ss-wake").await;
    let mut bob = InProcNode::join(&alice.swarm, "wake-bob").await;

    alice.send("link").await;
    assert!(bob.wait_body("link", MSG_TIMEOUT).await, "bob meshed");

    alice
        .apply_patch(json!([{"op": "add", "path": "/turn", "value": "bob"}]))
        .await;

    // F8: bob is woken with the document.
    assert!(
        bob.wait_state_change(MSG_TIMEOUT, |doc| doc["turn"] == "bob")
            .await,
        "a peer's change must wake the agent with the derived document (F8)"
    );
    // F5: alice saw no state change on her own wake channel.
    assert!(
        alice.state_changes().is_empty(),
        "an agent must not be woken on its own patch (F5), got {:?}",
        alice.state_changes()
    );

    alice.leave().await;
    bob.leave().await;
}

/// Unbounded log + windowed state anti-entropy: drive far more patches than one
/// digest window holds, then a late joiner — arriving after all traffic, so only
/// anti-entropy can reach it — reconciles the *whole* log across several windows
/// and derives the identical document.
///
/// Each patch sets an independent key (`/k0`, `/k1`, …), so the *set* of events
/// fully determines the document regardless of replay order — the property under
/// test is completeness of backfill, not causal ordering.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn late_joiner_backfills_a_log_larger_than_one_window() {
    // Comfortably more than two digest windows (`ANTIENTROPY_DIGEST_WINDOW_IDS`
    // is 70), so the rolling older-window cursor is genuinely exercised.
    const PATCHES: usize = 160;

    let alice = InProcNode::create("ss-backfill").await;
    let early = InProcNode::join(&alice.swarm, "bf-early").await;
    // Mesh so the appends go out live (the creator's outbound buffer stays
    // empty) — the late joiner can then only be served by anti-entropy.
    alice.send("link").await;

    for index in 0..PATCHES {
        alice
            .apply_patch(json!([{"op": "add", "path": format!("/k{index}"), "value": index}]))
            .await;
    }

    let want = alice.state_document().await;
    assert_eq!(
        want.as_object().map(serde_json::Map::len),
        Some(PATCHES),
        "creator's own document holds every key it set"
    );

    // The early (meshed) peer converges via the live path.
    assert!(
        wait_doc(&early, RECOVERY_TIMEOUT, |doc| doc == &want).await,
        "early peer never converged on the live path"
    );

    // The late joiner arrives after all traffic — windowed anti-entropy, across
    // multiple rounds, must reconstruct the full log.
    let late = InProcNode::join(&alice.swarm, "bf-late").await;
    assert!(
        wait_doc(&late, Duration::from_secs(150), |doc| doc == &want).await,
        "late joiner never backfilled the full log via windowed anti-entropy ({} of {PATCHES} keys)",
        late.state_document()
            .await
            .as_object()
            .map_or(0, serde_json::Map::len)
    );

    alice.leave().await;
    early.leave().await;
    late.leave().await;
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
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_agents_ping_pong_via_shared_state() {
    // Total moves across both agents (each wake must fire for the loop to finish).
    const MOVES: usize = 6;

    let mut alice = InProcNode::create("ss-pp").await;
    let mut bob = InProcNode::join(&alice.swarm, "pp-bob").await;

    alice.send("link").await;
    assert!(bob.wait_body("link", MSG_TIMEOUT).await, "bob meshed");

    // alice opens with move 1 under its own key — no container to seed first.
    alice
        .apply_patch(json!([{"op": "add", "path": "/m1", "value": "alice"}]))
        .await;

    // Alternate: the mover for move `target` is bob when even, alice when odd.
    // Each waits until it sees the peer's previous move land (the document then
    // holds `target - 1` moves) — a real wake — then records its own.
    for target in 2..=MOVES {
        let author = if target % 2 == 0 { "bob" } else { "alice" };
        let patch = json!([{"op": "add", "path": format!("/m{target}"), "value": author}]);
        if target % 2 == 0 {
            assert!(
                bob.wait_state_change(MSG_TIMEOUT, |doc| move_count(doc) == target - 1)
                    .await,
                "bob not woken to make move {target}"
            );
            bob.apply_patch(patch).await;
        } else {
            assert!(
                alice
                    .wait_state_change(MSG_TIMEOUT, |doc| move_count(doc) == target - 1)
                    .await,
                "alice not woken to make move {target}"
            );
            alice.apply_patch(patch).await;
        }
    }

    // Both converge to the same document of exactly `MOVES` moves (no lost or
    // doubled move — the wake-driven alternation produced one move per step).
    let converged = |doc: &Value| move_count(doc) == MOVES;
    assert!(
        wait_doc(&alice, RECOVERY_TIMEOUT, converged).await,
        "alice did not converge to {MOVES} moves"
    );
    assert!(
        wait_doc(&bob, RECOVERY_TIMEOUT, converged).await,
        "bob did not converge to {MOVES} moves"
    );
    assert_eq!(
        alice.state_document().await,
        bob.state_document().await,
        "both agents must derive the byte-identical shared document"
    );

    alice.leave().await;
    bob.leave().await;
}

/// Compare-and-set (`--if-doc-hash`): a patch guarded by a stale document hash
/// is rejected without mutating; the same patch with the current hash applies.
/// This is the optimistic-concurrency guard that stops a concurrent peer's
/// change from being silently clobbered. The guard is local to the author, so a
/// single node exercises it fully.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cas_rejects_stale_hash_and_accepts_current() {
    let node = InProcNode::create("ss-cas").await;
    // Each patch adds a *distinct* key, so the fold is order-independent — the
    // test exercises the CAS guard, not the `(timestamp, id)` tie-break that a
    // same-key replace within one second would expose.
    node.apply_patch(json!([{"op": "add", "path": "/seed", "value": "x"}]))
        .await;

    let before = node.state_document().await;
    let hash = agent_habilis_swarm::document_hash(&before);

    // A guard hash that doesn't match the current document is rejected, and the
    // document is left unchanged.
    let stale = node
        .try_apply_patch_if(
            json!([{"op": "add", "path": "/a", "value": 1}]),
            Some("deadbeef".to_owned()),
        )
        .await;
    assert!(stale.is_err(), "stale-hash patch must be rejected");
    assert!(
        stale.unwrap_err().to_string().contains("stale document"),
        "rejection names the cause"
    );
    assert_eq!(
        node.state_document().await,
        before,
        "a rejected CAS patch must not mutate the document"
    );

    // The same patch with the *current* hash applies.
    node.try_apply_patch_if(
        json!([{"op": "add", "path": "/a", "value": 1}]),
        Some(hash.clone()),
    )
    .await
    .expect("current-hash patch applies");
    assert_eq!(node.state_document().await["a"], json!(1));

    // The now-superseded hash is stale and re-rejected.
    let after = node
        .try_apply_patch_if(json!([{"op": "add", "path": "/b", "value": 2}]), Some(hash))
        .await;
    assert!(after.is_err(), "a superseded hash must be rejected");

    node.leave().await;
}
