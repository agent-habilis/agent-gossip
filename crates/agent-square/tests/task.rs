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

use agent_square::{TaskId, TaskState};
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
    let mut bob = InProcNode::join(&alice.mesh, "t-create-bob").await;
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
    let mut bob = InProcNode::join(&alice.mesh, "t-life-bob").await;
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
    // …and so does the worker's own record. Asserting only at completion would
    // let a half-regression through, where `working` is dropped but `completed`
    // still lands.
    assert_eq!(
        served_state(&alice, "t-life-bob", &task_id).await,
        "TASK_STATE_WORKING",
        "the worker's own record never left submitted"
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

    assert_eq!(
        served_state(&alice, "t-life-bob", &task_id).await,
        "TASK_STATE_COMPLETED",
        "the worker's own record never went terminal"
    );

    alice.leave().await;
    bob.leave().await;
}

/// The state the **worker** serves from its own registry for `task_id`. The
/// worker is the A2A *server*, so this — not the initiator's mirror — is the
/// authoritative record, and it is the one a worker's own status leg has to
/// reach. Pre-fix it never did: the worker sealed its own status frame to the
/// addressee and then fed that unreadable frame back to its own state machine,
/// so `GetTask` answered `working` for an approved task and the idle sweep reaped
/// finished work. Asserting only the initiator's mirror (as the lifecycle test
/// once did) cannot see any of that.
async fn served_state(initiator: &InProcNode, worker: &str, task_id: &TaskId) -> String {
    let served = initiator
        .a2a_call(
            worker,
            "GetTask",
            serde_json::json!({ "id": task_id.as_str() }),
        )
        .await;
    served["result"]["status"]["state"]
        .as_str()
        .unwrap_or_else(|| panic!("GetTask returned no state: {served}"))
        .to_owned()
}

/// A status leg carries its state **in the body**, so it is the only leg kind the
/// sealed-self-echo bug could reach — and the only one that can pin the *sharded*
/// `ingest_own_leg` call site. A note over `MAX_MESSAGE_SIZE` splits the leg, so
/// this drives `broadcast_task_frame`'s multipart branch, which self-ingests
/// separately from the single-frame one.
///
/// A large *artifact* cannot substitute: the artifact ingest branch never reads
/// the body, so it advances the record identically whether it is handed the
/// sealed frame or the plaintext twin. Such a test passes with the bug present.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn own_sharded_status_leg_completes_the_workers_own_record() {
    let alice = InProcNode::create("t-shard").await;
    let mut bob = InProcNode::join(&alice.mesh, "t-shard-bob").await;
    alice.send("warmup").await;
    assert!(bob.wait_body("warmup", MSG_TIMEOUT).await, "mesh formed");

    let task_id = task_id_of(&alice.create_task("t-shard-bob", "big note").await);
    assert!(bob.wait_task_message(TASK_WAIT).await, "bob saw the brief");

    // Comfortably past MAX_MESSAGE_SIZE (3840), so the leg must split.
    let note = "x".repeat(8 * 1024);
    bob.task_status(&task_id, TaskState::Completed, Some(&note))
        .await;

    assert_eq!(
        served_state(&alice, "t-shard-bob", &task_id).await,
        "TASK_STATE_COMPLETED",
        "the worker's own record never went terminal on the sharded path"
    );

    alice.leave().await;
    bob.leave().await;
}

/// `CancelTask` was broken by the same sealed-self-echo defect, and worse: the
/// canceller emits a sealed `canceled` status, fed its own unreadable frame back,
/// and never advanced its own record — then `rpc.rs` built the RPC *response*
/// from that same un-advanced record. So the reply told its caller the task was
/// not canceled, while the peer was told it was. Both halves are asserted here:
/// the response, and the record it is served from.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_advances_the_cancellers_own_record_and_response() {
    let alice = InProcNode::create("t-cancel").await;
    let mut bob = InProcNode::join(&alice.mesh, "t-cancel-bob").await;
    alice.send("warmup").await;
    assert!(bob.wait_body("warmup", MSG_TIMEOUT).await, "mesh formed");

    let task_id = task_id_of(&alice.create_task("t-cancel-bob", "never mind").await);
    assert!(bob.wait_task_message(TASK_WAIT).await, "bob saw the brief");

    // `CancelTask` is served by whoever *receives* the RPC, so alice is the
    // canceller: she emits the `canceled` status and answers from her own record.
    let alice_nick = alice.nickname.clone();
    let canceled = bob
        .a2a_call(
            &alice_nick,
            "CancelTask",
            serde_json::json!({ "id": task_id.as_str() }),
        )
        .await;
    assert_eq!(
        canceled["result"]["status"]["state"], "TASK_STATE_CANCELED",
        "CancelTask's response must report the task canceled: {canceled}"
    );
    // And the record it was served from. Asserting bob's copy instead would prove
    // nothing: the *peer* of a cancel always converged correctly — it is the
    // canceller's own record that never advanced.
    assert_eq!(
        served_state(&bob, &alice_nick, &task_id).await,
        "TASK_STATE_CANCELED",
        "the canceller's own record never went terminal"
    );

    alice.leave().await;
    bob.leave().await;
}

/// Two tasks created on the same worker get **distinct** server-minted ids and
/// advance independently.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_tasks_get_distinct_ids() {
    let alice = InProcNode::create("t-two").await;
    let mut bob = InProcNode::join(&alice.mesh, "t-two-bob").await;
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
