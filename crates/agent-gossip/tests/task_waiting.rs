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
    alice.broadcast("warmup").await;
    assert!(bob.wait_body("warmup", MSG_TIMEOUT).await, "mesh formed");

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

    bob.task_waiting(&task_id, "asks its user which branch")
        .await;
    assert!(
        alice.wait_task_state(TaskState::Working, MSG_TIMEOUT).await,
        "initiator never saw the working leg"
    );
    let metadata = served_metadata(&alice, "t-wait-bob", &task_id).await;
    assert_eq!(
        metadata["mesh:waiting"], "asks its user which branch",
        "got: {metadata}"
    );

    bob.task_status(&task_id, TaskState::Working, Some("back at it"))
        .await;
    let cleared = served_metadata(&alice, "t-wait-bob", &task_id).await;
    assert!(
        cleared.get("mesh:waiting").is_none(),
        "the next status must clear the reason, got: {cleared}"
    );

    alice.leave().await;
    bob.leave().await;
}
