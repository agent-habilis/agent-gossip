//! Handover behavioral tests (in-process, via the `InProcNode` harness).
//!
//! A handover is a directed, typed exchange (`MessageKind::Handover`) that
//! transfers a task brief from one participant to another. Surfacing follows
//! the directed-`Msg` precedent: a leg is shown and logged only by its
//! addressee and the sender's own echo — a third party relays it without
//! surfacing or retaining. These tests pin that contract end-to-end through
//! the real event loop (real iroh mesh, no subprocess), so the captured
//! `OutputEvent::Handover` form is byte-identical to the `--output json`
//! wire the skills consume.
//!
//! The CLI/stdout/Unix-socket wire contract (the exact `{"event":"handover"}`
//! line, the unknown-participant error, and `ahs peers`) lives in
//! `monitor_contract.rs`; the MCP surface in `mcp_stdio.rs`.

mod common;

use std::time::Duration;

use agent_habilis_swarm::{ExchangeId, ExchangeKind, ExchangePhase, MessageKind, OutputEvent};
use common::{InProcNode, MSG_TIMEOUT, three_peers};

/// A fixed, valid task id for these in-process tests. Each test runs an
/// isolated mesh, so one literal id is fine; a single exchange's legs all
/// share it (the correlation contract).
fn exchange_id() -> ExchangeId {
    "550e8400-e29b-41d4-a716-446655440000"
        .parse()
        .expect("valid task id")
}

/// Budget for a handover leg to fan out and be surfaced by its addressee.
/// A handover leg is an ordinary steady-state delivery, so it tracks the
/// suite's standard (`common::MSG_TIMEOUT`) rather than carrying its own value
/// that could drift from it.
const HANDOVER_WAIT: Duration = MSG_TIMEOUT;

/// A task leg addressed to a peer is surfaced by that peer (and only that
/// peer), carrying the brief in the body and the `to`/`exchange_id`/`kind`/
/// `phase` fields.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn task_offer_surfaces_to_addressee() {
    let mut creator = InProcNode::create("ho-direct").await;
    let swarm = creator.swarm.clone();
    let mut joiner = InProcNode::join(&swarm, "ho-bob").await;
    let tid = exchange_id();

    // Mesh first: the addressee must be a known participant before offer.
    assert!(
        creator.wait_presence("ho-bob", true, HANDOVER_WAIT).await,
        "creator should see ho-bob join before offering"
    );

    let brief = "## Task\nport the JSON parser to serde";
    creator
        .exchange(
            "ho-bob",
            &tid,
            ExchangeKind::Handover,
            ExchangePhase::Offer,
            brief,
        )
        .await
        .expect("task offer within rate limit");

    assert!(
        joiner
            .wait_exchange(ExchangePhase::Offer, HANDOVER_WAIT)
            .await,
        "addressee should surface the task offer"
    );
    let legs = joiner.exchanges();
    let (msg, is_self) = legs
        .iter()
        .find(|(msg, _)| matches!(&msg.kind, MessageKind::Exchange { phase, .. } if *phase == ExchangePhase::Offer))
        .expect("offer leg captured");
    assert!(!is_self, "the addressee's copy is not a self-echo");
    assert_eq!(msg.author.as_str(), creator.nickname.as_str());
    assert_eq!(msg.body.as_str(), brief);
    let MessageKind::Exchange {
        to,
        exchange_id: got_id,
        kind,
        phase,
    } = &msg.kind
    else {
        panic!("expected Task kind, got {:?}", msg.kind);
    };
    assert_eq!(to.as_str(), "ho-bob");
    assert_eq!(got_id.as_str(), tid.as_str());
    assert_eq!(*kind, ExchangeKind::Handover);
    assert_eq!(*phase, ExchangePhase::Offer);

    joiner.leave().await;
    creator.leave().await;
}

/// The sender surfaces its own task leg as a self-echo (`is_self`), so the
/// `/swarm:handover` skill can confirm the send.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn task_self_echoes_to_sender() {
    let mut creator = InProcNode::create("ho-echo").await;
    let swarm = creator.swarm.clone();
    let joiner = InProcNode::join(&swarm, "ho-bob").await;
    assert!(creator.wait_presence("ho-bob", true, HANDOVER_WAIT).await);

    creator
        .exchange(
            "ho-bob",
            &exchange_id(),
            ExchangeKind::Handover,
            ExchangePhase::Offer,
            "brief",
        )
        .await
        .expect("within rate limit");

    assert!(
        creator
            .wait_exchange(ExchangePhase::Offer, HANDOVER_WAIT)
            .await,
        "sender should see its own task echo"
    );
    let legs = creator.exchanges();
    assert!(
        legs.iter().any(|(msg, is_self)| *is_self
            && matches!(&msg.kind, MessageKind::Exchange { phase, .. } if *phase == ExchangePhase::Offer)),
        "the sender's copy is flagged is_self"
    );

    joiner.leave().await;
    creator.leave().await;
}

/// A third party in the swarm relays the task (gossip floods) but never
/// surfaces it and never logs it — `poll`/`fetch` for the bystander must
/// not contain the leg. This is the directed-`Msg` privacy contract.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn task_not_surfaced_to_third_party() {
    let (mut creator, mut addressee, mut bystander) = three_peers("ho-third").await;
    let addressee_nick = addressee.nickname.clone();

    // Everyone meshed: the creator must know the addressee to send `offer`.
    assert!(
        creator
            .wait_presence(&addressee_nick, true, HANDOVER_WAIT)
            .await
    );

    creator
        .exchange(
            &addressee_nick,
            &exchange_id(),
            ExchangeKind::Handover,
            ExchangePhase::Offer,
            "secret brief",
        )
        .await
        .expect("within rate limit");

    // Addressee surfaces it…
    assert!(
        addressee
            .wait_exchange(ExchangePhase::Offer, HANDOVER_WAIT)
            .await,
        "addressee should surface the task"
    );

    // …the bystander, given equal time, never does, and never logs it.
    let bystander_saw = bystander
        .wait_exchange(ExchangePhase::Offer, HANDOVER_WAIT)
        .await;
    assert!(
        !bystander_saw,
        "a third party must never surface a task addressed to someone else"
    );
    let logged = bystander
        .session
        .fetch(None)
        .await
        .expect("fetch")
        .into_iter()
        .any(|item| matches!(item.event, OutputEvent::Exchange { .. }));
    assert!(
        !logged,
        "a third party must not log/retain a task addressed to someone else"
    );

    bystander.leave().await;
    addressee.leave().await;
    creator.leave().await;
}

/// A `progress` leg is plumbing: it surfaces to the addressee as a task
/// event (rendered `exchange_progress`) but is **never logged** — it must not
/// appear in the addressee's poll/fetch buffer (unlike content legs).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exchange_progress_is_plumbing_not_logged() {
    let mut alice = InProcNode::create("ho-prog").await;
    let swarm = alice.swarm.clone();
    let mut bob = InProcNode::join(&swarm, "ho-bob").await;
    let alice_nick = alice.nickname.clone();
    assert!(
        bob.wait_presence(alice_nick.as_str(), true, HANDOVER_WAIT)
            .await
    );

    bob.exchange(
        &alice_nick,
        &exchange_id(),
        ExchangeKind::Task,
        ExchangePhase::Progress,
        "35/100",
    )
    .await
    .expect("progress beat");

    // The addressee surfaces the progress leg as a task event…
    assert!(
        alice
            .wait_exchange(ExchangePhase::Progress, HANDOVER_WAIT)
            .await,
        "addressee should surface the progress beat"
    );
    // …but it is plumbing — never retained in the poll/fetch buffer.
    let logged = alice
        .session
        .fetch(None)
        .await
        .expect("fetch")
        .into_iter()
        .any(|item| {
            matches!(
                item.event,
                OutputEvent::Exchange { msg, .. }
                    if matches!(&msg.kind, MessageKind::Exchange { phase, .. } if *phase == ExchangePhase::Progress)
            )
        });
    assert!(!logged, "a progress beat must never be logged/retained");

    bob.leave().await;
    alice.leave().await;
}

/// A full offer → accept → context → done → confirm exchange surfaces every
/// leg to the two parties, in both directions, all under one `exchange_id`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn task_full_exchange_round_trips() {
    let mut alice = InProcNode::create("ho-full").await;
    let swarm = alice.swarm.clone();
    let mut bob = InProcNode::join(&swarm, "ho-bob").await;
    assert!(alice.wait_presence("ho-bob", true, HANDOVER_WAIT).await);
    assert!(
        bob.wait_presence(alice.nickname.as_str(), true, HANDOVER_WAIT)
            .await
    );

    let alice_nick = alice.nickname.clone();
    let tid = exchange_id();
    let kind = ExchangeKind::Handover;

    // A → B offer, B → A accept (entry), A → B context, B → A done, A → B confirm.
    alice
        .exchange("ho-bob", &tid, kind, ExchangePhase::Offer, "brief")
        .await
        .expect("offer");
    assert!(bob.wait_exchange(ExchangePhase::Offer, HANDOVER_WAIT).await);

    bob.exchange(
        &alice_nick,
        &tid,
        kind,
        ExchangePhase::Accept,
        "taking it on",
    )
    .await
    .expect("accept");
    assert!(
        alice
            .wait_exchange(ExchangePhase::Accept, HANDOVER_WAIT)
            .await
    );

    alice
        .exchange(
            "ho-bob",
            &tid,
            kind,
            ExchangePhase::Context,
            "all parser tests green",
        )
        .await
        .expect("context");
    assert!(
        bob.wait_exchange(ExchangePhase::Context, HANDOVER_WAIT)
            .await
    );

    bob.exchange(
        &alice_nick,
        &tid,
        kind,
        ExchangePhase::Done,
        "ported; verify with `cargo test parser`",
    )
    .await
    .expect("done");
    assert!(
        alice
            .wait_exchange(ExchangePhase::Done, HANDOVER_WAIT)
            .await
    );

    alice
        .exchange(
            "ho-bob",
            &tid,
            kind,
            ExchangePhase::Confirm,
            "verified, thanks",
        )
        .await
        .expect("confirm");
    assert!(
        bob.wait_exchange(ExchangePhase::Confirm, HANDOVER_WAIT)
            .await
    );

    bob.leave().await;
    alice.leave().await;
}
