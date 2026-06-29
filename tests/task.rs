//! `task` behavioral tests (kind=task on the exchange mechanism) (in-process, via the `InProcNode` harness).
//!
//! `/swarm:task` sends one or more `task`-kind exchanges: the worker runs the work
//! and reports its **result** on the `done` leg, and the initiator `confirm`s.
//! These pin the two properties the skill relies on through the real event
//! loop (real iroh mesh, no subprocess): a full task round trip carries the
//! result back to the initiator, and several tasks run as **independent**
//! `exchange_id`s with no cross-task coupling (there is no group barrier in the
//! daemon — each task is its own exchange).
//!
//! The generic task wire contract (directed-privacy, self-echo, `progress`
//! plumbing) is covered kind-agnostically in `handover.rs`; here we exercise
//! the task-specific `done(result) → confirm` path and exchange-id isolation.

mod common;

use std::time::Duration;

use agent_habilis_swarm::{ExchangeId, ExchangeKind, ExchangePhase, MessageKind};
use common::{InProcNode, MSG_TIMEOUT, three_peers};

const TASK_WAIT: Duration = MSG_TIMEOUT;

fn exchange_id_a() -> ExchangeId {
    "550e8400-e29b-41d4-a716-446655440000"
        .parse()
        .expect("valid task id")
}

fn exchange_id_b() -> ExchangeId {
    "660e8400-e29b-41d4-a716-446655440001"
        .parse()
        .expect("valid task id")
}

/// The body of the captured task leg of `phase` (from any author), or `None`.
fn leg_body(node: &mut InProcNode, phase: ExchangePhase) -> Option<String> {
    node.exchanges()
        .iter()
        .find(|(msg, _)| matches!(&msg.kind, MessageKind::Exchange { phase: got, .. } if *got == phase))
        .map(|(msg, _)| msg.body.as_str().to_owned())
}

/// A full task round trip `offer → accept → done(result) → confirm` carries
/// the worker's **result** back to the initiator on the `done` leg.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn task_round_trips_result_to_initiator() {
    let mut initiator = InProcNode::create("ex-rt").await;
    let swarm = initiator.swarm.clone();
    let mut worker = InProcNode::join(&swarm, "ex-worker").await;
    let initiator_nick = initiator.nickname.clone();
    let tid = exchange_id_a();
    let kind = ExchangeKind::Task;

    assert!(
        initiator.wait_presence("ex-worker", true, TASK_WAIT).await,
        "initiator should see the worker join before offering"
    );

    // offer (with explicit done-criteria in the brief) → accept.
    let brief = "## Task\nreview src/net\n## Done when\nfindings reported";
    initiator
        .exchange("ex-worker", &tid, kind, ExchangePhase::Offer, brief)
        .await
        .expect("offer sent");
    assert!(worker.wait_exchange(ExchangePhase::Offer, TASK_WAIT).await);

    worker
        .exchange(&initiator_nick, &tid, kind, ExchangePhase::Accept, "on it")
        .await
        .expect("accept");
    assert!(
        initiator
            .wait_exchange(ExchangePhase::Accept, TASK_WAIT)
            .await
    );

    // The worker reports its result on `done`.
    let result = "2 findings: missing await in recv; unbounded buffer in flush";
    worker
        .exchange(&initiator_nick, &tid, kind, ExchangePhase::Done, result)
        .await
        .expect("done");
    assert!(
        initiator
            .wait_exchange(ExchangePhase::Done, TASK_WAIT)
            .await
    );
    assert_eq!(
        leg_body(&mut initiator, ExchangePhase::Done).as_deref(),
        Some(result),
        "the initiator must receive the worker's result on the done leg"
    );

    // The initiator accepts the result.
    initiator
        .exchange("ex-worker", &tid, kind, ExchangePhase::Confirm, "thanks")
        .await
        .expect("confirm");
    assert!(
        worker
            .wait_exchange(ExchangePhase::Confirm, TASK_WAIT)
            .await
    );

    worker.leave().await;
    initiator.leave().await;
}

/// Two tasks fanned to two different workers run as independent
/// exchanges: each worker surfaces only the offer addressed to it, and the
/// initiator collects both distinct results, each under its own `exchange_id`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_tasks_are_independent() {
    let (mut initiator, mut worker_a, mut worker_b) = three_peers("ex-indep").await;
    let initiator_nick = initiator.nickname.clone();
    let nick_a = worker_a.nickname.clone();
    let nick_b = worker_b.nickname.clone();
    let (tid_a, tid_b) = (exchange_id_a(), exchange_id_b());
    let kind = ExchangeKind::Task;

    assert!(initiator.wait_presence(&nick_a, true, TASK_WAIT).await);
    assert!(initiator.wait_presence(&nick_b, true, TASK_WAIT).await);

    // Two offers, one per worker, each its own exchange_id and brief.
    initiator
        .exchange(
            &nick_a,
            &tid_a,
            kind,
            ExchangePhase::Offer,
            "review src/net",
        )
        .await
        .expect("offer a");
    initiator
        .exchange(
            &nick_b,
            &tid_b,
            kind,
            ExchangePhase::Offer,
            "review src/daemon",
        )
        .await
        .expect("offer b");

    assert!(
        worker_a
            .wait_exchange(ExchangePhase::Offer, TASK_WAIT)
            .await
    );
    assert!(
        worker_b
            .wait_exchange(ExchangePhase::Offer, TASK_WAIT)
            .await
    );

    // Each worker surfaces only its own exchange_id, never the other's.
    let a_ids: Vec<String> = worker_a
        .exchanges()
        .iter()
        .filter_map(|(msg, _)| {
            if let MessageKind::Exchange { exchange_id, .. } = &msg.kind {
                Some(exchange_id.as_str().to_owned())
            } else {
                None
            }
        })
        .collect();
    assert!(a_ids.contains(&tid_a.as_str().to_owned()));
    assert!(
        !a_ids.contains(&tid_b.as_str().to_owned()),
        "worker_a must not surface the task addressed to worker_b"
    );

    // Each worker accepts and reports a distinct result; the initiator
    // collects both, correlated by exchange_id.
    worker_a
        .exchange(&initiator_nick, &tid_a, kind, ExchangePhase::Accept, "ok")
        .await
        .expect("accept a");
    worker_b
        .exchange(&initiator_nick, &tid_b, kind, ExchangePhase::Accept, "ok")
        .await
        .expect("accept b");

    let (result_a, result_b) = ("net: 1 finding", "daemon: 3 findings");
    worker_a
        .exchange(&initiator_nick, &tid_a, kind, ExchangePhase::Done, result_a)
        .await
        .expect("done a");
    worker_b
        .exchange(&initiator_nick, &tid_b, kind, ExchangePhase::Done, result_b)
        .await
        .expect("done b");

    // Both done legs arrive; the initiator can tell them apart by exchange_id.
    assert!(
        initiator
            .wait_for(TASK_WAIT, |events| {
                let dones: Vec<(&str, &str)> = events
                    .iter()
                    .filter_map(|event| {
                        let agent_habilis_swarm::OutputEvent::Exchange { msg, .. } = event else {
                            return None;
                        };
                        if let MessageKind::Exchange {
                            exchange_id,
                            phase: ExchangePhase::Done,
                            ..
                        } = &msg.kind
                        {
                            Some((exchange_id.as_str(), msg.body.as_str()))
                        } else {
                            None
                        }
                    })
                    .collect();
                dones.contains(&(tid_a.as_str(), result_a))
                    && dones.contains(&(tid_b.as_str(), result_b))
            })
            .await,
        "the initiator must collect both workers' results, each under its own exchange_id"
    );

    worker_b.leave().await;
    worker_a.leave().await;
    initiator.leave().await;
}
