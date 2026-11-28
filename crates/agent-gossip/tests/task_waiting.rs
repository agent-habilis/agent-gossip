//! Integration tests for the waiting-on-user flag of a task.

use agent_gossip::a2a::status_update_params::Progress;
use agent_gossip::{TaskId, TaskState};
use agent_gossip_test_fixtures as common;
use common::{InProcNode, MSG_TIMEOUT, task_id_of};

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

/// A worker that waits on its own user says so on a `working` check-in. The
/// worker's registry serves the flag as `mesh:waiting_user`, a check-in that
/// repeats it keeps it, and the next check-in without it clears it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_task_shows_waiting_user_until_a_check_in_without_it() {
    let mut alice = InProcNode::create("t-wait").await;
    let mut bob = InProcNode::join(&alice.mesh, "t-wait-bob").await;
    common::warm_up(&alice, &mut [&mut bob]).await;

    let response = alice.create_task("t-wait-bob", "port the parser").await;
    let task_id = task_id_of(&response);
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

    bob.task_waiting_user(
        &task_id,
        "still waiting for the answer",
        Progress::try_from(50).expect("50 is a percentage"),
    )
    .await;
    let repeated = served_metadata(&alice, "t-wait-bob", &task_id).await;
    assert_eq!(
        repeated["mesh:waiting_user"], true,
        "a check-in that repeats the flag must keep it, got: {repeated}"
    );

    bob.task_checkin(
        &task_id,
        "back at it",
        Progress::try_from(50).expect("50 is a percentage"),
    )
    .await;
    let cleared = served_metadata(&alice, "t-wait-bob", &task_id).await;
    assert!(
        cleared.get("mesh:waiting_user").is_none(),
        "a check-in without the flag must clear it, got: {cleared}"
    );

    alice.leave().await;
    bob.leave().await;
}
