//! Integration tests for the waiting-on tag of a task.

use agent_gossip::a2a::status_update_params::Progress;
use agent_gossip::{TaskId, TaskState};
use agent_gossip_test_fixtures as common;
use common::{InProcNode, MSG_TIMEOUT};

async fn served_metadata(
    initiator: &InProcNode,
    worker: &str,
    task_id: &TaskId,
) -> serde_json::Value {
    let served = initiator
        .a2a_call(
            worker,
            "GetTask",
            serde_json::json!({ "id": task_id.as_str() }),
        )
        .await;
    served["result"]["metadata"].clone()
}

/// A worker that waits on its own user says so on a `working` leg. The
/// worker's registry serves the reason under `mesh:waiting`, and the next
/// status leg clears it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_task_shows_the_waiting_reason_until_the_next_status() {
    let mut alice = InProcNode::create("t-wait").await;
    let mut bob = InProcNode::join(&alice.mesh, "t-wait-bob").await;
    common::warm_up(&alice, &mut [&mut bob]).await;

    let response = alice.create_task("t-wait-bob", "port the parser").await;
    let task_id: TaskId = response["result"]["task"]["id"]
        .as_str()
        .expect("the worker returned a Task with an id")
        .parse()
        .expect("the returned id is a valid task id");
    assert!(
        bob.wait_task_message(MSG_TIMEOUT).await,
        "bob saw the brief"
    );

    bob.task_waiting_user(
        &task_id,
        "asks its user which branch",
        Progress::try_from(50).expect("50 is a percentage"),
    )
    .await;
    assert!(
        alice.wait_task_state(TaskState::Working, MSG_TIMEOUT).await,
        "initiator never saw the working leg"
    );
    let metadata = served_metadata(&alice, "t-wait-bob", &task_id).await;
    assert_eq!(metadata["mesh:waiting_user"], true, "got: {metadata}");

    bob.task_checkin(
        &task_id,
        "back at it",
        Progress::try_from(50).expect("50 is a percentage"),
    )
    .await;
    let cleared = served_metadata(&alice, "t-wait-bob", &task_id).await;
    assert!(
        cleared.get("mesh:waiting_user").is_none(),
        "the next status must clear the flag, got: {cleared}"
    );

    alice.leave().await;
    bob.leave().await;
}
