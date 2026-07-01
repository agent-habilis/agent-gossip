//! `task` behavioral tests (in-process, via the `InProcNode` harness).
//!
//! A task is a directed, phased conversation (`MessageKind::Task`) correlated
//! by `task_id`. Two families of tests live here: the generic wire contract
//! (a leg is surfaced only by its addressee and the sender's own echo — the
//! directed-`Msg` privacy precedent — plus the self-echo and `progress`
//! plumbing), and the task-specific `done(result) → confirm` path and
//! task-id isolation. All run through the real event loop (real iroh mesh,
//! no subprocess), so the captured `OutputEvent::Task` form is byte-identical
//! to the `--output json` wire the skills consume.
//!
//! The CLI/stdout/Unix-socket wire contract (the exact `{"event":"task"}`
//! line, the unknown-participant error, and `ahsw peers`) lives in
//! `monitor_contract.rs`; the MCP surface in `mcp_stdio.rs`.

mod common;

use std::time::Duration;

use agent_habilis_swarm::{MessageKind, OutputEvent, TaskId, TaskPhase};
use common::{InProcNode, MSG_TIMEOUT, three_peers};

const TASK_WAIT: Duration = MSG_TIMEOUT;

fn task_id_a() -> TaskId {
    "550e8400-e29b-41d4-a716-446655440000"
        .parse()
        .expect("valid task id")
}

fn task_id_b() -> TaskId {
    "660e8400-e29b-41d4-a716-446655440001"
        .parse()
        .expect("valid task id")
}

/// The body of the captured task leg of `phase` (from any author), or `None`.
fn leg_body(node: &mut InProcNode, phase: TaskPhase) -> Option<String> {
    node.tasks()
        .iter()
        .find(|(msg, _)| matches!(&msg.kind, MessageKind::Task { phase: got, .. } if *got == phase))
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
    let tid = task_id_a();

    assert!(
        initiator.wait_presence("ex-worker", true, TASK_WAIT).await,
        "initiator should see the worker join before offering"
    );

    // offer (with explicit done-criteria in the brief) → accept.
    let brief = "## Task\nreview src/net\n## Done when\nfindings reported";
    initiator
        .task("ex-worker", &tid, TaskPhase::Offer, brief)
        .await
        .expect("offer sent");
    assert!(worker.wait_task(TaskPhase::Offer, TASK_WAIT).await);

    worker
        .task(&initiator_nick, &tid, TaskPhase::Accept, "on it")
        .await
        .expect("accept");
    assert!(initiator.wait_task(TaskPhase::Accept, TASK_WAIT).await);

    // The worker reports its result on `done`.
    let result = "2 findings: missing await in recv; unbounded buffer in flush";
    worker
        .task(&initiator_nick, &tid, TaskPhase::Done, result)
        .await
        .expect("done");
    assert!(initiator.wait_task(TaskPhase::Done, TASK_WAIT).await);
    assert_eq!(
        leg_body(&mut initiator, TaskPhase::Done).as_deref(),
        Some(result),
        "the initiator must receive the worker's result on the done leg"
    );

    // The initiator accepts the result.
    initiator
        .task("ex-worker", &tid, TaskPhase::Confirm, "thanks")
        .await
        .expect("confirm");
    assert!(worker.wait_task(TaskPhase::Confirm, TASK_WAIT).await);

    worker.leave().await;
    initiator.leave().await;
}

/// Two tasks fanned to two different workers run as independent
/// tasks: each worker surfaces only the offer addressed to it, and the
/// initiator collects both distinct results, each under its own `task_id`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_tasks_are_independent() {
    let (mut initiator, mut worker_a, mut worker_b) = three_peers("ex-indep").await;
    let initiator_nick = initiator.nickname.clone();
    let nick_a = worker_a.nickname.clone();
    let nick_b = worker_b.nickname.clone();
    let (tid_a, tid_b) = (task_id_a(), task_id_b());

    assert!(initiator.wait_presence(&nick_a, true, TASK_WAIT).await);
    assert!(initiator.wait_presence(&nick_b, true, TASK_WAIT).await);

    // Two offers, one per worker, each its own task_id and brief.
    initiator
        .task(&nick_a, &tid_a, TaskPhase::Offer, "review src/net")
        .await
        .expect("offer a");
    initiator
        .task(&nick_b, &tid_b, TaskPhase::Offer, "review src/daemon")
        .await
        .expect("offer b");

    assert!(worker_a.wait_task(TaskPhase::Offer, TASK_WAIT).await);
    assert!(worker_b.wait_task(TaskPhase::Offer, TASK_WAIT).await);

    // Each worker surfaces only its own task_id, never the other's.
    let a_ids: Vec<String> = worker_a
        .tasks()
        .iter()
        .filter_map(|(msg, _)| {
            if let MessageKind::Task { task_id, .. } = &msg.kind {
                Some(task_id.as_str().to_owned())
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
    // collects both, correlated by task_id.
    worker_a
        .task(&initiator_nick, &tid_a, TaskPhase::Accept, "ok")
        .await
        .expect("accept a");
    worker_b
        .task(&initiator_nick, &tid_b, TaskPhase::Accept, "ok")
        .await
        .expect("accept b");

    let (result_a, result_b) = ("net: 1 finding", "daemon: 3 findings");
    worker_a
        .task(&initiator_nick, &tid_a, TaskPhase::Done, result_a)
        .await
        .expect("done a");
    worker_b
        .task(&initiator_nick, &tid_b, TaskPhase::Done, result_b)
        .await
        .expect("done b");

    // Both done legs arrive; the initiator can tell them apart by task_id.
    assert!(
        initiator
            .wait_for(TASK_WAIT, |events| {
                let dones: Vec<(&str, &str)> = events
                    .iter()
                    .filter_map(|event| {
                        let OutputEvent::Task { msg, .. } = event else {
                            return None;
                        };
                        if let MessageKind::Task {
                            task_id,
                            phase: TaskPhase::Done,
                            ..
                        } = &msg.kind
                        {
                            Some((task_id.as_str(), msg.body.as_str()))
                        } else {
                            None
                        }
                    })
                    .collect();
                dones.contains(&(tid_a.as_str(), result_a))
                    && dones.contains(&(tid_b.as_str(), result_b))
            })
            .await,
        "the initiator must collect both workers' results, each under its own task_id"
    );

    worker_b.leave().await;
    worker_a.leave().await;
    initiator.leave().await;
}

/// A task leg addressed to a peer is surfaced by that peer (and only that
/// peer), carrying the brief in the body and the `to`/`task_id`/`phase`
/// fields.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn task_offer_surfaces_to_addressee() {
    let mut creator = InProcNode::create("ho-direct").await;
    let swarm = creator.swarm.clone();
    let mut joiner = InProcNode::join(&swarm, "ho-bob").await;
    let tid = task_id_a();

    // Mesh first: the addressee must be a known participant before offer.
    assert!(
        creator.wait_presence("ho-bob", true, TASK_WAIT).await,
        "creator should see ho-bob join before offering"
    );

    let brief = "## Task\nport the JSON parser to serde";
    creator
        .task("ho-bob", &tid, TaskPhase::Offer, brief)
        .await
        .expect("task offer sent");

    assert!(
        joiner.wait_task(TaskPhase::Offer, TASK_WAIT).await,
        "addressee should surface the task offer"
    );
    let legs = joiner.tasks();
    let (msg, is_self) = legs
        .iter()
        .find(|(msg, _)| matches!(&msg.kind, MessageKind::Task { phase, .. } if *phase == TaskPhase::Offer))
        .expect("offer leg captured");
    assert!(!is_self, "the addressee's copy is not a self-echo");
    assert_eq!(msg.author.as_str(), creator.nickname.as_str());
    assert_eq!(msg.body.as_str(), brief);
    let MessageKind::Task {
        to,
        task_id: got_id,
        phase,
    } = &msg.kind
    else {
        panic!("expected Task kind, got {:?}", msg.kind);
    };
    assert_eq!(to.as_str(), "ho-bob");
    assert_eq!(got_id.as_str(), tid.as_str());
    assert_eq!(*phase, TaskPhase::Offer);

    joiner.leave().await;
    creator.leave().await;
}

/// The sender surfaces its own task leg as a self-echo (`is_self`), so the
/// `/swarm:task` skill can confirm the send.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn task_self_echoes_to_sender() {
    let mut creator = InProcNode::create("ho-echo").await;
    let swarm = creator.swarm.clone();
    let joiner = InProcNode::join(&swarm, "ho-bob").await;
    assert!(creator.wait_presence("ho-bob", true, TASK_WAIT).await);

    creator
        .task("ho-bob", &task_id_a(), TaskPhase::Offer, "brief")
        .await
        .expect("leg sent");

    assert!(
        creator.wait_task(TaskPhase::Offer, TASK_WAIT).await,
        "sender should see its own task echo"
    );
    let legs = creator.tasks();
    assert!(
        legs.iter().any(|(msg, is_self)| *is_self
            && matches!(&msg.kind, MessageKind::Task { phase, .. } if *phase == TaskPhase::Offer)),
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
            .wait_presence(&addressee_nick, true, TASK_WAIT)
            .await
    );

    creator
        .task(
            &addressee_nick,
            &task_id_a(),
            TaskPhase::Offer,
            "secret brief",
        )
        .await
        .expect("leg sent");

    // Addressee surfaces it…
    assert!(
        addressee.wait_task(TaskPhase::Offer, TASK_WAIT).await,
        "addressee should surface the task"
    );

    // …the bystander, given equal time, never does, and never logs it.
    let bystander_saw = bystander.wait_task(TaskPhase::Offer, TASK_WAIT).await;
    assert!(
        !bystander_saw,
        "a third party must never surface a task addressed to someone else"
    );
    let logged = bystander
        .session
        .fetch(None, None)
        .await
        .expect("fetch")
        .into_iter()
        .any(|item| matches!(item.event, OutputEvent::Task { .. }));
    assert!(
        !logged,
        "a third party must not log/retain a task addressed to someone else"
    );

    bystander.leave().await;
    addressee.leave().await;
    creator.leave().await;
}

/// A `progress` leg is plumbing: it surfaces to the addressee as a task
/// event (rendered `task_progress`) but is **never logged** — it must not
/// appear in the addressee's poll/fetch buffer (unlike content legs).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn task_progress_is_plumbing_not_logged() {
    let mut alice = InProcNode::create("ho-prog").await;
    let swarm = alice.swarm.clone();
    let mut bob = InProcNode::join(&swarm, "ho-bob").await;
    let alice_nick = alice.nickname.clone();
    assert!(
        bob.wait_presence(alice_nick.as_str(), true, TASK_WAIT)
            .await
    );

    bob.task(&alice_nick, &task_id_a(), TaskPhase::Progress, "35/100")
        .await
        .expect("progress beat");

    // The addressee surfaces the progress leg as a task event…
    assert!(
        alice.wait_task(TaskPhase::Progress, TASK_WAIT).await,
        "addressee should surface the progress beat"
    );
    // …but it is plumbing — never retained in the poll/fetch buffer.
    let logged = alice
        .session
        .fetch(None, None)
        .await
        .expect("fetch")
        .into_iter()
        .any(|item| {
            matches!(
                item.event,
                OutputEvent::Task { msg, .. }
                    if matches!(&msg.kind, MessageKind::Task { phase, .. } if *phase == TaskPhase::Progress)
            )
        });
    assert!(!logged, "a progress beat must never be logged/retained");

    bob.leave().await;
    alice.leave().await;
}

/// A full offer → accept → context → done → confirm task surfaces every
/// leg to the two parties, in both directions, all under one `task_id`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn task_full_lifecycle_round_trips() {
    let mut alice = InProcNode::create("ho-full").await;
    let swarm = alice.swarm.clone();
    let mut bob = InProcNode::join(&swarm, "ho-bob").await;
    assert!(alice.wait_presence("ho-bob", true, TASK_WAIT).await);
    assert!(
        bob.wait_presence(alice.nickname.as_str(), true, TASK_WAIT)
            .await
    );

    let alice_nick = alice.nickname.clone();
    let tid = task_id_a();

    // A → B offer, B → A accept (entry), A → B context, B → A done, A → B confirm.
    alice
        .task("ho-bob", &tid, TaskPhase::Offer, "brief")
        .await
        .expect("offer");
    assert!(bob.wait_task(TaskPhase::Offer, TASK_WAIT).await);

    bob.task(&alice_nick, &tid, TaskPhase::Accept, "taking it on")
        .await
        .expect("accept");
    assert!(alice.wait_task(TaskPhase::Accept, TASK_WAIT).await);

    alice
        .task("ho-bob", &tid, TaskPhase::Context, "all parser tests green")
        .await
        .expect("context");
    assert!(bob.wait_task(TaskPhase::Context, TASK_WAIT).await);

    bob.task(
        &alice_nick,
        &tid,
        TaskPhase::Done,
        "ported; verify with `cargo test parser`",
    )
    .await
    .expect("done");
    assert!(alice.wait_task(TaskPhase::Done, TASK_WAIT).await);

    alice
        .task("ho-bob", &tid, TaskPhase::Confirm, "verified, thanks")
        .await
        .expect("confirm");
    assert!(bob.wait_task(TaskPhase::Confirm, TASK_WAIT).await);

    bob.leave().await;
    alice.leave().await;
}
