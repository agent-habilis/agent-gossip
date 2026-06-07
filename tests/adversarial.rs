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

use std::time::Duration;

use agent_habilis_swarm::OutputEvent;
use agent_habilis_swarm::harness::adversarial::{self, CraftedMsg};
use common::InProcNode;

const T: Duration = Duration::from_secs(30);

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

#[tokio::test]
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

#[tokio::test]
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

#[tokio::test]
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

#[tokio::test]
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

#[tokio::test]
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

#[tokio::test]
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

#[tokio::test]
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

#[tokio::test]
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
