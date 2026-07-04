//! Native A2A task lifecycle tests (in-process, via the `InProcNode`
//! harness). A task is created by a directed `message/send` over the gossip
//! request/response binding — the **worker** mints the id and returns the
//! `Task` — then advanced by worker-pushed `TaskStatusUpdate` /
//! `TaskArtifactUpdate` frames (the A2A streaming plane) and initiator
//! `message/send` follow-ups, and closed by the **worker's** `completed`
//! status after the initiator approves. All run through the real event loop
//! (real iroh mesh, no subprocess).
//!
//! The CLI/stdout/Unix-socket wire contract lives in `monitor_contract.rs`;
//! the gossip request/response mechanics in `a2a_rpc.rs`.

mod common;

use std::time::Duration;

use agent_gossip::{TaskId, TaskState};
use common::{InProcNode, MSG_TIMEOUT, three_peers};

const TASK_WAIT: Duration = MSG_TIMEOUT;

/// Parse the `Task` id out of a `SendMessage` response (v1.0
/// `SendMessageResponse` oneof: `{"result":{"task":<Task>}}`).
fn task_id_of(resp: &serde_json::Value) -> TaskId {
    resp["result"]["task"]["id"]
        .as_str()
        .expect("the worker returned a Task with an id")
        .parse()
        .expect("the returned id is a valid task id")
}

/// Creating a task is a directed `message/send` with no `taskId`: the worker
/// mints a fresh **server** id and returns a `submitted` `Task`, and surfaces
/// the incoming message to its own skill.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn creation_mints_server_id_and_returns_submitted_task() {
    let alice = InProcNode::create("t-create").await;
    let mut bob = InProcNode::join(&alice.swarm, "t-create-bob").await;
    alice.send("warmup").await;
    assert!(bob.wait_body("warmup", MSG_TIMEOUT).await, "mesh formed");

    let resp = alice.create_task("t-create-bob", "review src/net").await;
    assert!(resp["result"]["task"].is_object(), "got: {resp}");
    assert_eq!(
        resp["result"]["task"]["status"]["state"],
        "TASK_STATE_SUBMITTED"
    );
    // The id is server-minted — a fresh uuid, not derived from our message.
    let task_id = task_id_of(&resp);
    assert!(!task_id.as_str().is_empty());

    // The worker's skill sees the incoming brief as a `message`-kind task event.
    assert!(
        bob.wait_task_message(TASK_WAIT).await,
        "the worker never surfaced the incoming task message"
    );

    alice.leave().await;
    bob.leave().await;
}

/// The full native lifecycle: create → worker `working` → worker artifact
/// (parks in `input-required`) → initiator approval message → worker
/// `completed`. The initiator observes each worker push; the worker authors
/// the terminal `completed`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_lifecycle_worker_completes_after_approval() {
    let mut alice = InProcNode::create("t-life").await;
    let mut bob = InProcNode::join(&alice.swarm, "t-life-bob").await;
    alice.send("warmup").await;
    assert!(bob.wait_body("warmup", MSG_TIMEOUT).await, "mesh formed");

    let resp = alice.create_task("t-life-bob", "port the parser").await;
    let task_id = task_id_of(&resp);
    assert!(bob.wait_task_message(TASK_WAIT).await, "bob saw the brief");

    // Worker accepts (working) → initiator sees it.
    bob.task_status(&task_id, TaskState::Working, Some("on it"))
        .await;
    assert!(
        alice.wait_task_state(TaskState::Working, TASK_WAIT).await,
        "initiator never saw the worker accept"
    );

    // Worker returns a result (artifact) → parks in input-required for review.
    bob.task_artifact(&task_id, "here is the port").await;
    assert!(
        alice
            .wait_task_state(TaskState::InputRequired, TASK_WAIT)
            .await,
        "initiator never saw the result / review park"
    );

    // Initiator approves via a SendMessage follow-up.
    let approve = alice.task_message("t-life-bob", &task_id, "approved").await;
    assert!(
        approve["result"]["task"].is_object(),
        "approval acked: {approve}"
    );

    // The worker authors the terminal `completed`.
    bob.task_status(&task_id, TaskState::Completed, Some("done"))
        .await;
    assert!(
        alice.wait_task_state(TaskState::Completed, TASK_WAIT).await,
        "initiator never saw the worker complete the task"
    );

    alice.leave().await;
    bob.leave().await;
}

/// Two tasks created on the same worker get **distinct** server-minted ids and
/// advance independently.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_tasks_get_distinct_ids() {
    let alice = InProcNode::create("t-two").await;
    let mut bob = InProcNode::join(&alice.swarm, "t-two-bob").await;
    alice.send("warmup").await;
    assert!(bob.wait_body("warmup", MSG_TIMEOUT).await, "mesh formed");

    let first = task_id_of(&alice.create_task("t-two-bob", "task one").await);
    let second = task_id_of(&alice.create_task("t-two-bob", "task two").await);
    assert_ne!(first, second, "each task gets its own server-minted id");

    alice.leave().await;
    bob.leave().await;
}

/// A task is private to its two parties: a third peer relays the traffic but
/// never surfaces a task event for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn not_surfaced_to_third_party() {
    let (alice, mut bob, mut gamma) = three_peers("t-priv").await;
    alice.send("warmup").await;
    assert!(
        bob.wait_body("warmup", MSG_TIMEOUT).await && gamma.wait_body("warmup", MSG_TIMEOUT).await,
        "mesh formed"
    );

    let resp = alice
        .create_task(bob.nickname.as_str(), "private work")
        .await;
    let task_id = task_id_of(&resp);
    bob.task_status(&task_id, TaskState::Working, None).await;
    assert!(
        bob.wait_task_message(TASK_WAIT).await,
        "the worker is a party and sees the task"
    );

    // Give gamma a delivery barrier: a later broadcast it *does* see.
    alice.send("barrier").await;
    assert!(
        gamma.wait_body("barrier", MSG_TIMEOUT).await,
        "gamma meshed"
    );
    assert!(
        gamma.tasks().is_empty() && !gamma.saw_task_message(),
        "an uninvolved third party must never surface another pair's task"
    );

    alice.leave().await;
    bob.leave().await;
    gamma.leave().await;
}
