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

use agent_gossip_test_fixtures as common;

use std::time::{Duration, Instant};

use agent_gossip::OutputEvent;
use agent_gossip::harness::adversarial::{self, CraftedMsg};
use common::{InProcNode, MSG_TIMEOUT, POLL};
use serde_json::{Value, json};

// Delivery-barrier budget. These tests assert a message *is* delivered (so
// the adversarial one was dropped / the gap surfaces); the fast path passes
// in well under a second. It is an ordinary in-process delivery, so it tracks
// the suite's steady-state standard (`common::MSG_TIMEOUT`) for the headroom a
// loaded debug-build host needs — see that constant's note.
const T: Duration = MSG_TIMEOUT;

/// A victim + attacker pair on a fresh loopback mesh, **meshed** (a warmup
/// from the attacker is observed by the victim) so injected bytes are
/// actually delivered. The victim is the observer.
async fn meshed_pair(tag: &str) -> (InProcNode, InProcNode) {
    let mut victim = InProcNode::create(&format!("adv-{tag}")).await;
    let attacker = InProcNode::join(&victim.mesh, &format!("adv-{tag}-atk")).await;
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
    let evil = CraftedMsg::new(attacker.session.mesh_id(), "ghost", "evil-unsigned").bytes();
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
    let evil = CraftedMsg::new(attacker.session.mesh_id(), "ghost", "honest")
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

/// The kind-demotion attack: sign a msg, then rewrite its kind to `notice`
/// (or the reverse) on the wire. The kind is part of the canonical bytes, so
/// the signature no longer verifies and the victim drops it — a relay can
/// never turn a no-auto-reply notice into an auto-replyable msg.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kind_flipped_after_signing_is_dropped() {
    let (mut victim, attacker) = meshed_pair("kindflip").await;
    let key = adversarial::new_key();
    let evil = CraftedMsg::new(attacker.session.mesh_id(), "ghost", "flip-me")
        .sign(&key)
        .flip_chat_kind()
        .bytes();
    attacker.session.inject_raw(evil).await.expect("inject");
    attacker.send("barrier-kindflip").await;
    assert!(
        victim.wait_body("barrier-kindflip", T).await,
        "barrier lost"
    );
    assert!(
        !surfaced(&mut victim, "flip-me"),
        "a kind-flipped message must be dropped (signature mismatch)"
    );
    victim.leave().await;
    attacker.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn equivocation_surfaces_a_fork() {
    let (mut victim, attacker) = meshed_pair("fork").await;
    let key = adversarial::new_key();
    let mesh = attacker.session.mesh_id();
    // Same key, same seq, two different bodies → two valid but conflicting
    // signed messages: cryptographic proof of equivocation.
    let first = CraftedMsg::new(mesh, "two-face", "fork-a")
        .wrap_a2a()
        .chain(7, None)
        .sign(&key)
        .bytes();
    let second = CraftedMsg::new(mesh, "two-face", "fork-b")
        .wrap_a2a()
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
    let mesh = attacker.session.mesh_id();
    let shared_id = "550e8400-e29b-41d4-a716-446655440000";

    // 1) An UNSIGNED message with a chosen id — dropped at the signature gate,
    //    and (post-fix) never recorded as "seen".
    let forged = CraftedMsg::new(mesh, "ghost", "forged")
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
    let genuine = CraftedMsg::new(mesh, "ghost", "genuine")
        .id(shared_id)
        .wrap_a2a()
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
async fn signed_forgery_with_replayed_id_does_not_suppress_victim() {
    // The signature gate stops an *unsigned* id-replay, but a peer can sign its
    // OWN message reusing a victim's id — a valid signature over its own bytes.
    // Dedup must key on `(pubkey, id)` (not the bare id), so this forgery lands
    // under a different key and cannot suppress the victim's genuine message
    // that shares the id.
    let (mut receiver, injector) = meshed_pair("dedup-key").await;
    let victim_key = adversarial::new_key();
    let attacker_key = adversarial::new_key();
    let mesh = injector.session.mesh_id();
    let shared_id = "550e8400-e29b-41d4-a716-446655440000";

    // 1) A validly SIGNED forgery under the attacker's key, reusing the id.
    let forged = CraftedMsg::new(mesh, "attacker", "forged")
        .id(shared_id)
        .sign(&attacker_key)
        .bytes();
    injector
        .session
        .inject_raw(forged)
        .await
        .expect("inject signed forgery");
    injector.send("after-forged").await;
    assert!(receiver.wait_body("after-forged", T).await, "barrier lost");

    // 2) The victim's genuine message reusing that id must still be delivered —
    //    the forgery's dedup key differs, so it never marked the id "seen".
    let genuine = CraftedMsg::new(mesh, "victim", "genuine")
        .id(shared_id)
        .wrap_a2a()
        .sign(&victim_key)
        .bytes();
    injector
        .session
        .inject_raw(genuine)
        .await
        .expect("inject genuine");
    assert!(
        receiver.wait_body("genuine", T).await,
        "a signed forgery reusing an id must not suppress the victim's genuine message"
    );
    receiver.leave().await;
    injector.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn junk_payload_chat_is_dropped() {
    let (mut victim, attacker) = meshed_pair("junk-payload").await;
    let key = adversarial::new_key();
    // Validly signed, but the body is raw text, not a serialized A2A Message.
    let evil = CraftedMsg::new(attacker.session.mesh_id(), "ghost", "raw-not-a2a")
        .sign(&key)
        .bytes();
    attacker.session.inject_raw(evil).await.expect("inject");
    attacker.send("barrier-junk").await;
    assert!(victim.wait_body("barrier-junk", T).await, "barrier lost");
    assert!(
        !surfaced(&mut victim, "raw-not-a2a"),
        "a signed chat frame with a non-A2A body must be dropped at the boundary"
    );
    victim.leave().await;
    attacker.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn payload_frame_id_mismatch_is_dropped() {
    let (mut victim, attacker) = meshed_pair("id-mismatch").await;
    let key = adversarial::new_key();
    // Wrap a valid payload first, then re-id the FRAME — the payload's
    // messageId no longer names the frame, a mismatch only a crafted client
    // produces.
    let evil = CraftedMsg::new(attacker.session.mesh_id(), "ghost", "id-mismatch-body")
        .wrap_a2a()
        .id("00000000-0000-0000-0000-00000000beef")
        .sign(&key)
        .bytes();
    attacker.session.inject_raw(evil).await.expect("inject");
    attacker.send("barrier-mismatch").await;
    assert!(
        victim.wait_body("barrier-mismatch", T).await,
        "barrier lost"
    );
    assert!(
        !surfaced(&mut victim, "id-mismatch-body"),
        "a payload whose messageId contradicts the frame id must be dropped"
    );
    victim.leave().await;
    attacker.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_task_id_mismatch_is_dropped() {
    // An a2a_status frame whose payload taskId contradicts the frame's
    // correlation field — only a crafted client produces this; the boundary
    // gate must drop it before it can touch the task machine or surface.
    let (mut victim, attacker) = meshed_pair("status-mismatch").await;
    let key = adversarial::new_key();
    let victim_nick = victim.nickname.clone();
    let evil = CraftedMsg::status_frame(
        attacker.session.mesh_id(),
        adversarial::StatusFrameParams {
            author: "ghost",
            to: &victim_nick,
            frame_task_id: "550e8400-e29b-41d4-a716-446655440000", // frame correlation
            payload_task_id: "660e8400-e29b-41d4-a716-446655440001", // payload taskId — mismatch
        },
    )
    .sign(&key)
    .bytes();
    attacker.session.inject_raw(evil).await.expect("inject");
    attacker.send("barrier-status").await;
    assert!(victim.wait_body("barrier-status", T).await, "barrier lost");
    let surfaced_status = victim
        .events()
        .iter()
        .any(|event| matches!(event, OutputEvent::Task { .. }));
    assert!(
        !surfaced_status,
        "a status whose payload taskId contradicts the frame must be dropped"
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
    let evil = CraftedMsg::new(attacker.session.mesh_id(), "time-lord", "from-the-future")
        .wrap_a2a()
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
    let evil = CraftedMsg::new(attacker.session.mesh_id(), &victim_nick, "i-am-you")
        .wrap_a2a()
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

/// True if `event` is a delivered `Message` whose visible text equals
/// `body` — used to check whether a given sybil identity's flood message
/// was accepted.
fn is_flood_message(event: &OutputEvent, body: &str) -> bool {
    matches!(
        event,
        OutputEvent::Message { msg, is_self: false }
            if agent_gossip::a2a::gossip::chat_text(msg).as_deref() == Some(body)
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[should_panic(expected = "OPEN GAP")]
async fn gap_sybil_identities_are_accepted() {
    // Five messages from five brand-new keys. There is no membership control
    // and no cross-identity limit, so all are accepted — a sybil can mint
    // unlimited valid identities.
    let (mut victim, attacker) = meshed_pair("sybil").await;
    let mesh = attacker.session.mesh_id();
    for index in 0..5u32 {
        let key = adversarial::new_key();
        let bytes = CraftedMsg::new(mesh, &format!("sybil-{index}"), &format!("flood-{index}"))
            .wrap_a2a()
            .sign(&key)
            .bytes();
        attacker.session.inject_raw(bytes).await.expect("inject");
    }
    let all_accepted = victim
        .wait_for(T, |events| {
            (0..5u32).all(|index| {
                let body = format!("flood-{index}");
                events.iter().any(|event| is_flood_message(event, &body))
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
    let evil =
        CraftedMsg::state_merge(attacker.session.mesh_id(), "ghost", json!({"evil": true})).bytes();
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_foreign_card_forgery_is_rejected_on_receipt() {
    // A malicious peer that bypasses its own author-side gate can still SIGN and
    // inject a meta change forging another member's `/peers/<nick>/card` — the
    // card carries that member's cryptographic identity. Every honest receiver
    // must reject it before it folds, so the forgery converges nowhere.
    let (victim, injector) = meshed_pair("card-forge").await;
    let attacker_key = adversarial::new_key();
    let mesh = injector.session.mesh_id();
    let victim_nick = victim.nickname.as_str().to_owned();
    let fake = "ff00beefff00beef"; // distinctive marker for the forged identity

    let forged = CraftedMsg::meta_forge_card(
        mesh,
        "attacker",
        &victim_nick,
        &json!({"metadata": {"pubkey": fake}}),
    )
    .sign(&attacker_key)
    .bytes();
    injector
        .session
        .inject_raw(forged)
        .await
        .expect("inject forged card");

    // Barrier: a genuine meta write from the injector's own subtree (not a card,
    // so ungated). Once the victim derives it, the forged frame — sent first on
    // the same link — has had its turn.
    injector
        .meta_merge(json!({"peers": {injector.nickname.as_str(): {"barrier": true}}}))
        .await;
    let barrier_ptr = format!("/peers/{}/barrier", injector.nickname);
    let deadline = Instant::now() + T;
    while victim.meta_get().await.pointer(&barrier_ptr) != Some(&json!(true)) {
        assert!(
            Instant::now() < deadline,
            "barrier meta merge never derived"
        );
        tokio::time::sleep(POLL).await;
    }

    assert!(
        !victim.meta_get().await.to_string().contains(fake),
        "a signed forged foreign card must be rejected on receipt, never folded"
    );
    victim.leave().await;
    injector.leave().await;
}

// ── Shard reassembly: crafted headers + byte budgets ────────────────────

/// A crafted shard with an out-of-range header (`idx >= total`, or a `total`
/// past the sanity tripwire) is rejected at `Message::parse` and never
/// reaches the reassembly store.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn out_of_range_shard_headers_never_reach_the_store() {
    const GROUP: &str = "550e8400-e29b-41d4-a716-446655440000";
    let (mut victim, attacker) = meshed_pair("shard-hdr").await;
    let key = adversarial::new_key();
    // idx outside total.
    let idx_outside = CraftedMsg::new(attacker.session.mesh_id(), "ghost", "slice")
        .shard(GROUP, 3, 3)
        .sign(&key)
        .bytes();
    attacker
        .session
        .inject_raw(idx_outside)
        .await
        .expect("inject");
    // total past the header tripwire.
    let absurd_total = CraftedMsg::new(attacker.session.mesh_id(), "ghost", "slice")
        .shard(GROUP, 0, agent_gossip::MAX_SHARD_TOTAL + 1)
        .sign(&key)
        .bytes();
    attacker
        .session
        .inject_raw(absurd_total)
        .await
        .expect("inject");

    attacker.send("barrier-shard-hdr").await;
    assert!(
        victim.wait_body("barrier-shard-hdr", T).await,
        "barrier lost"
    );
    let (groups, bytes, _) = victim
        .session
        .reassembly_stats()
        .await
        .expect("reassembly stats");
    assert_eq!(
        (groups, bytes),
        (0, 0),
        "parse-rejected shards must never be buffered"
    );
    victim.leave().await;
    attacker.leave().await;
}

/// A Sybil stream of never-completing shard groups (many fresh keys, each
/// buffering slices of a group that can never finish) is bounded by the
/// store's byte budgets — the crafted stream buffers (proving the shards were
/// accepted), but the accounting stays inside the per-author and global
/// ceilings.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sybil_shard_floods_stay_inside_the_reassembly_budgets() {
    let (mut victim, attacker) = meshed_pair("shard-sybil").await;
    let slice = "x".repeat(3000);
    for author in 0..24u32 {
        let key = adversarial::new_key();
        let group = format!("00000000-0000-4000-8000-{author:012x}");
        // idx 0..4 of a claimed 1000: the group can never complete, so every
        // accepted slice sits in the store until the TTL sweep.
        for idx in 0..4u32 {
            let evil = CraftedMsg::new(
                attacker.session.mesh_id(),
                &format!("sybil-{author}"),
                &slice,
            )
            .shard(&group, idx, 1000)
            .sign(&key)
            .bytes();
            attacker.session.inject_raw(evil).await.expect("inject");
        }
    }

    attacker.send("barrier-shard-sybil").await;
    assert!(
        victim.wait_body("barrier-shard-sybil", T).await,
        "barrier lost"
    );
    let (groups, total_bytes, max_author_bytes) = victim
        .session
        .reassembly_stats()
        .await
        .expect("reassembly stats");
    assert!(groups > 0, "the crafted shards were accepted and buffered");
    assert!(
        total_bytes <= adversarial::REASSEMBLY_TOTAL_BUDGET_BYTES,
        "global reassembly budget exceeded: {total_bytes}"
    );
    assert!(
        max_author_bytes <= adversarial::REASSEMBLY_AUTHOR_BUDGET_BYTES,
        "per-author reassembly budget exceeded: {max_author_bytes}"
    );
    victim.leave().await;
    attacker.leave().await;
}

/// A crafted shard reusing a *victim-authored* group id forms its own
/// (attacker-keyed) set: it can never fill the victim's slots, so the genuine
/// body reassembles intact — the cross-author isolation boundary, end to end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cross_author_group_reuse_cannot_corrupt_a_genuine_body() {
    let (mut victim, attacker) = meshed_pair("shard-xauth").await;
    // The attacker seeds a poisoned slice under a group id it hopes a genuine
    // multipart send will use — then the sender (attacker node's session,
    // signing with its real key) sends a genuine multipart body. The poisoned
    // slice is under a different key, so it cannot enter the genuine set.
    let key = adversarial::new_key();
    let poisoned = CraftedMsg::new(attacker.session.mesh_id(), "ghost", "POISON")
        .shard("00000000-0000-4000-8000-00000000beef", 1, 2)
        .sign(&key)
        .bytes();
    attacker.session.inject_raw(poisoned).await.expect("inject");

    // A genuine multipart body from the attacker's real session (its own
    // group id — the A2A messageId — differs, but even a collision would be
    // keyed apart). It must arrive intact, with no poisoned slice.
    let big = "clean ".repeat(4 * 1024); // ~24 KB, several shards
    attacker.send(&big).await;
    assert!(
        victim.wait_body(&big, T).await,
        "the genuine multipart body never reassembled"
    );
    assert!(
        !surfaced(&mut victim, "POISON"),
        "a poisoned cross-author slice must never surface"
    );
    victim.leave().await;
    attacker.leave().await;
}
