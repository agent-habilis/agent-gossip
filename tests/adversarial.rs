//! Adversarial / malicious-peer integration suite (requires `--features
//! adversarial`; `cargo task test`/`ci` enable it). A real in-process attacker
//! node broadcasts **crafted** wire bytes via the injector — messages
//! a correct client would never produce — and we assert how a victim node
//! reacts over the real iroh mesh.
//!
//! Two kinds of test:
//! - **Defended** (`must pass`): the properties Phase 1–3 actually enforce —
//!   unsigned/tampered messages are dropped, equivocation surfaces a `fork`.
//! - **Open-gap tripwires** (`#[should_panic(expected = "OPEN GAP")]`):
//!   adversarial scenarios we do **not** yet defend against. Each asserts the
//!   defense we *lack*; that assert currently fails (panics), which
//!   `should_panic` captures — so the test is **green today** and flips to
//!   **red the moment someone closes the gap**, forcing it to be updated.
//!
//! Determinism (no flaky tests): every scenario confirms the mesh with a
//! warmup, and negative assertions use a *delivery barrier* — a real message
//! sent from the same node after the injection — so absence means "dropped",
//! never "not yet arrived".

mod common;

use std::time::{Duration, Instant};

use agent_habilis_swarm::OutputEvent;
use agent_habilis_swarm::harness::adversarial::{self, CraftedMsg};
use common::{InProcNode, MSG_TIMEOUT, POLL};
use serde_json::{Value, json};

// Delivery-barrier budget. These tests assert a message *is* delivered (so
// the adversarial one was dropped / the gap surfaces); the fast path passes
// in well under a second. It is an ordinary in-process delivery, so it tracks
// the suite's steady-state standard (`common::MSG_TIMEOUT`) for the headroom a
// loaded debug-build host needs — see that constant's note.
const T: Duration = MSG_TIMEOUT;

/// A victim + attacker pair on a fresh loopback swarm, **meshed** (a warmup
/// from the attacker is observed by the victim) so injected bytes are
/// actually delivered. The victim is the observer.
async fn meshed_pair(tag: &str) -> (InProcNode, InProcNode) {
    let mut victim = InProcNode::create(&format!("adv-{tag}")).await;
    let attacker = InProcNode::join(&victim.swarm, &format!("adv-{tag}-atk")).await;
    attacker.send("warmup").await;
    assert!(
        victim.wait_body("warmup", T).await,
        "victim/attacker never meshed"
    );
    (victim, attacker)
}

/// True if the victim ever surfaced a peer `msg` with this body.
fn surfaced(victim: &mut InProcNode, body: &str) -> bool {
    victim.count_body(body) > 0
}

// ── Defended: these MUST pass ──────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsigned_message_is_dropped() {
    let (mut victim, attacker) = meshed_pair("unsigned").await;
    // No `.sign(..)` → empty signature. The victim must reject it.
    let evil = CraftedMsg::new(attacker.session.swarm_id(), "ghost", "evil-unsigned").bytes();
    attacker.session.inject_raw(evil).await.expect("inject");
    // Barrier: a real signed message from the same sender, sent *after* the
    // injection. When it arrives, the unsigned one (sent first) had its turn.
    attacker.send("barrier-unsigned").await;
    assert!(
        victim.wait_body("barrier-unsigned", T).await,
        "barrier lost"
    );
    assert!(
        !surfaced(&mut victim, "evil-unsigned"),
        "unsigned message must be dropped, never surfaced"
    );
    victim.leave().await;
    attacker.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tampered_message_is_dropped() {
    let (mut victim, attacker) = meshed_pair("tampered").await;
    let key = adversarial::new_key();
    // Sign "honest", then mutate the body → signature no longer matches.
    let evil = CraftedMsg::new(attacker.session.swarm_id(), "ghost", "honest")
        .sign(&key)
        .tamper_body("tampered-after-sign")
        .bytes();
    attacker.session.inject_raw(evil).await.expect("inject");
    attacker.send("barrier-tampered").await;
    assert!(
        victim.wait_body("barrier-tampered", T).await,
        "barrier lost"
    );
    assert!(
        !surfaced(&mut victim, "tampered-after-sign") && !surfaced(&mut victim, "honest"),
        "a tampered message must be dropped (signature mismatch)"
    );
    victim.leave().await;
    attacker.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn equivocation_surfaces_a_fork() {
    let (mut victim, attacker) = meshed_pair("fork").await;
    let key = adversarial::new_key();
    let swarm = attacker.session.swarm_id();
    // Same key, same seq, two different bodies → two valid but conflicting
    // signed messages: cryptographic proof of equivocation.
    let first = CraftedMsg::new(swarm, "two-face", "fork-a")
        .chain(7, None)
        .sign(&key)
        .bytes();
    let second = CraftedMsg::new(swarm, "two-face", "fork-b")
        .chain(7, None)
        .sign(&key)
        .bytes();
    attacker.session.inject_raw(first).await.expect("inject a");
    attacker.session.inject_raw(second).await.expect("inject b");
    let forked = victim
        .wait_for(T, |events| {
            events
                .iter()
                .any(|event| matches!(event, OutputEvent::Fork { seq, .. } if *seq == 7))
        })
        .await;
    assert!(
        forked,
        "two signed messages at the same seq must surface a fork event"
    );
    victim.leave().await;
    attacker.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forged_message_does_not_suppress_genuine_with_replayed_id() {
    // Authenticity must be checked BEFORE dedup: an unsigned/forged message
    // carrying a replayed id must not poison the dedup window and suppress the
    // genuine signed copy that shares that id.
    let (mut victim, attacker) = meshed_pair("dedup-order").await;
    let key = adversarial::new_key();
    let swarm = attacker.session.swarm_id();
    let shared_id = "550e8400-e29b-41d4-a716-446655440000";

    // 1) An UNSIGNED message with a chosen id — dropped at the signature gate,
    //    and (post-fix) never recorded as "seen".
    let forged = CraftedMsg::new(swarm, "ghost", "forged")
        .id(shared_id)
        .bytes();
    attacker
        .session
        .inject_raw(forged)
        .await
        .expect("inject forged");
    // Barrier (same link, in order) so the victim has processed the forged copy
    // before the genuine one arrives — makes the regression deterministic.
    attacker.send("after-forged").await;
    assert!(victim.wait_body("after-forged", T).await, "barrier lost");

    // 2) A genuine SIGNED message reusing that id must still be delivered.
    let genuine = CraftedMsg::new(swarm, "ghost", "genuine")
        .id(shared_id)
        .sign(&key)
        .bytes();
    attacker
        .session
        .inject_raw(genuine)
        .await
        .expect("inject genuine");
    assert!(
        victim.wait_body("genuine", T).await,
        "a genuine signed message with a replayed id must still be delivered"
    );
    victim.leave().await;
    attacker.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn directed_replies_to_third_party_do_not_leak_into_indexes() {
    // A reply addressed to someone else is relayed but never logged. It must
    // therefore never be folded into the fork/DAG indexes — otherwise those
    // maps grow without bound (the leak). Only the two open messages from the
    // attacker's own key are retained and indexed.
    let (mut victim, attacker) = meshed_pair("noleak").await;
    let key = adversarial::new_key();
    let swarm = attacker.session.swarm_id();

    for index in 0..20u32 {
        let bytes = CraftedMsg::new(swarm, "ghost", &format!("to-other-{index}"))
            .reply_to("someone-else")
            .chain(u64::from(index), None)
            .sign(&key)
            .bytes();
        attacker
            .session
            .inject_raw(bytes)
            .await
            .expect("inject reply");
    }
    // Barrier: an open message that IS logged + indexed, and confirms the 20
    // replies (injected first, same link) were already processed.
    attacker.send("barrier").await;
    assert!(victim.wait_body("barrier", T).await, "barrier lost");

    let (by_hash, _dag_heads, author_seqs) =
        victim.session.index_stats().await.expect("index stats");
    assert_eq!(
        author_seqs, 1,
        "only the attacker's own key is indexed; replies to a third party must not leak (was 2 before the fix)"
    );
    assert!(
        by_hash <= 2,
        "only the warmup + barrier open messages are indexed, not the 20 directed replies: by_hash={by_hash}"
    );
    victim.leave().await;
    attacker.leave().await;
}

// ── Open-gap tripwires: scenarios we are currently OPEN TO ─────────────
//
// Green today (the defensive assert fails → panics → should_panic catches
// it). When a gap is closed the assert holds, no panic fires, and the test
// goes RED — convert it to a positive assertion then.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[should_panic(expected = "OPEN GAP")]
async fn gap_future_timestamp_is_accepted() {
    // A validly-signed message with an absurd FUTURE timestamp and no parents
    // (nothing to bound it). We only enforce `ts >= max(parents.ts)`; with no
    // parents there is no check, and there is no absolute-time sanity bound.
    let (mut victim, attacker) = meshed_pair("future-ts").await;
    let key = adversarial::new_key();
    let evil = CraftedMsg::new(attacker.session.swarm_id(), "time-lord", "from-the-future")
        .timestamp(4_102_444_800) // 2100-01-01
        .sign(&key)
        .bytes();
    attacker.session.inject_raw(evil).await.expect("inject");
    let seen = victim.wait_body("from-the-future", T).await;
    assert!(
        !seen,
        "OPEN GAP: a far-future timestamp is accepted (no absolute-time bound)"
    );
    victim.leave().await;
    attacker.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[should_panic(expected = "OPEN GAP")]
async fn gap_nickname_impersonation_is_accepted() {
    // The attacker posts under the *victim's* display nickname but with its
    // own key. Names are cosmetic (not authenticated), so the message is
    // delivered & surfaced — a consumer trusting the name is fooled; only the
    // pubkey distinguishes them.
    let (mut victim, attacker) = meshed_pair("imposter").await;
    let victim_nick = victim.nickname.clone();
    let key = adversarial::new_key(); // NOT the victim's key
    let evil = CraftedMsg::new(attacker.session.swarm_id(), &victim_nick, "i-am-you")
        .sign(&key)
        .bytes();
    attacker.session.inject_raw(evil).await.expect("inject");
    let seen = victim.wait_body("i-am-you", T).await;
    assert!(
        !seen,
        "OPEN GAP: a message impersonating an existing nickname is accepted (names are not authenticated)"
    );
    victim.leave().await;
    attacker.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[should_panic(expected = "OPEN GAP")]
async fn gap_sybil_identities_are_accepted() {
    // Five messages from five brand-new keys. There is no membership control
    // and no cross-identity limit, so all are accepted — a sybil can mint
    // unlimited valid identities.
    let (mut victim, attacker) = meshed_pair("sybil").await;
    let swarm = attacker.session.swarm_id();
    for index in 0..5u32 {
        let key = adversarial::new_key();
        let bytes = CraftedMsg::new(swarm, &format!("sybil-{index}"), &format!("flood-{index}"))
            .sign(&key)
            .bytes();
        attacker.session.inject_raw(bytes).await.expect("inject");
    }
    let all_accepted = victim
        .wait_for(T, |events| {
            (0..5u32).all(|index| {
                let body = format!("flood-{index}");
                events.iter().any(|event| {
                    matches!(event, OutputEvent::Message { msg, is_self: false } if msg.body.as_str() == body)
                })
            })
        })
        .await;
    assert!(
        !all_accepted,
        "OPEN GAP: messages from many unauthorized fresh keys are all accepted (no membership / sybil control)"
    );
    victim.leave().await;
    attacker.leave().await;
}

// ── Defended: stream-end recovery (H1) ────────────────────────────────────────

/// The gossip event stream terminally ending must not leave the daemon
/// deaf: the heal arm re-subscribes the topic, the node re-enters the
/// overlay, and anti-entropy backfills what was missed during the
/// outage. Upstream closes a lagging subscriber outright (its docs:
/// "close and re-open"), so this is a realistic flood-pressure path.
/// `sever_gossip` flips the same `gossip_open` flag the real terminal
/// `None` arm does, so the loop cannot tell the difference.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn severed_gossip_stream_resubscribes_and_backfills() {
    let (mut victim, attacker) = meshed_pair("sever").await;
    victim.session.sever_gossip().await.expect("sever");

    // Broadcast while the victim's subscription is down: it must arrive
    // later via anti-entropy over the fresh subscription, not be lost.
    attacker.send("during-outage").await;

    // Recovery budget: one heal tick (fixed 15s) to resubscribe, the
    // re-announce/re-graft round-trip, then an anti-entropy cycle (10s)
    // for the backfill — with margin for a slow runner.
    let recovery = Duration::from_secs(75);
    let backfilled = victim.wait_body("during-outage", recovery).await;
    assert!(
        backfilled,
        "message broadcast during the outage never backfilled after resubscribe\nvictim events: {:#?}",
        victim.events()
    );

    // Live traffic flows again on the new subscription.
    attacker.send("after-recovery").await;
    assert!(
        victim.wait_body("after-recovery", T).await,
        "victim deaf to live traffic after resubscribe"
    );

    victim.leave().await;
    attacker.leave().await;
}

// ── Shared-state (Phase 1) adversarial cases ───────────────────────────
//
// State merges ride the same crypto gates as chat. A receiver must drop unsigned
// ones, and fold a *signed* but non-object merge as a deterministic no-op — never
// a panic, never a root replacement. The "barrier" here is a real, valid merge
// from the attacker's own identity: once the victim derives it, the crafted one
// (injected first) has had its turn.

/// Poll the victim's derived document until `pred` holds or `timeout` elapses.
async fn wait_doc(
    node: &InProcNode,
    timeout: Duration,
    mut pred: impl FnMut(&Value) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if pred(&node.state_get().await) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(POLL).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsigned_state_merge_is_dropped() {
    let (victim, attacker) = meshed_pair("state-unsigned").await;
    // A well-formed state merge, but UNSIGNED — must be dropped before it can
    // touch the state log (same authenticity gate as chat).
    let evil = CraftedMsg::state_merge(attacker.session.swarm_id(), "ghost", json!({"evil": true}))
        .bytes();
    attacker.session.inject_raw(evil).await.expect("inject");
    // Barrier: a real signed merge from the attacker's own identity.
    attacker.state_merge(json!({"ok": 1})).await;
    assert!(
        wait_doc(&victim, T, |doc| doc["ok"] == 1).await,
        "barrier state merge never derived"
    );
    assert!(
        victim.state_get().await.get("evil").is_none(),
        "an unsigned state merge must never be folded into the document"
    );
    victim.leave().await;
    attacker.leave().await;
}
