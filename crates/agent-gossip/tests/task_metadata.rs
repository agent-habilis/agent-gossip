//! Integration tests for what a worker's registry serves about a task, and for the rule that an
//! artifact carries something.

use agent_gossip::a2a::status_update_params::Progress;
use agent_gossip::{TaskId, TaskState};
use agent_gossip_test_fixtures as common;
use common::{InProcNode, MSG_TIMEOUT};

async fn served(
    initiator: &InProcNode,
    worker: &str,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    initiator.a2a_call(worker, method, params).await["result"].clone()
}

/// The worker's registry serves the last check-in: its text, its progress and the initiator's
/// label of the task, on `GetTask` and on `ListTasks` alike.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_task_and_list_tasks_serve_the_last_check_in() {
    let mut alice = InProcNode::create("t-meta").await;
    let mut bob = InProcNode::join(&alice.mesh, "t-meta-bob").await;
    common::warm_up(&alice, &mut [&mut bob]).await;

    let response = alice
        .create_task_labeled("t-meta-bob", "port the parser", Some("port parser"))
        .await;
    let task_id: TaskId = response["result"]["task"]["id"]
        .as_str()
        .expect("the worker returned a Task with an id")
        .parse()
        .expect("the returned id is a valid task id");
    assert!(
        bob.wait_task_message(MSG_TIMEOUT).await,
        "bob saw the brief"
    );
    bob.task_checkin(
        &task_id,
        "tests pass",
        Progress::try_from(60).expect("60 is a percentage"),
    )
    .await;
    assert!(
        alice.wait_task_state(TaskState::Working, MSG_TIMEOUT).await,
        "alice never saw the check-in"
    );

    let single = served(
        &alice,
        "t-meta-bob",
        "GetTask",
        serde_json::json!({ "id": task_id.as_str() }),
    )
    .await;
    let listed = served(&alice, "t-meta-bob", "ListTasks", serde_json::json!({})).await;
    let all = listed["tasks"]
        .as_array()
        .expect("ListTasks answers with a list");
    let in_list = all
        .iter()
        .find(|entry| entry["id"] == task_id.as_str())
        .expect("ListTasks names the task");
    for (via, view) in [("GetTask", &single), ("ListTasks", in_list)] {
        let metadata = &view["metadata"];
        assert_eq!(
            metadata["mesh:status_text"], "tests pass",
            "{via} must serve the text of the check-in, got: {metadata}"
        );
        assert_eq!(
            metadata["mesh:progress"], 60,
            "{via} must serve the progress as a percentage, got: {metadata}"
        );
        assert_eq!(
            metadata["mesh:label"], "port parser",
            "{via} must serve the label of the brief, got: {metadata}"
        );
        assert!(
            metadata.get("mesh:done").is_none() && metadata.get("mesh:total").is_none(),
            "{via} must not serve the retired fraction, got: {metadata}"
        );
    }

    alice.leave().await;
    bob.leave().await;
}

/// An artifact with no text and no file would park the task for review with nothing to review,
/// so the daemon refuses it and the task goes on working.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_artifact_is_refused_and_leaves_the_task_working() {
    let alice = InProcNode::create("t-empty").await;
    let mut bob = InProcNode::join(&alice.mesh, "t-empty-bob").await;
    common::warm_up(&alice, &mut [&mut bob]).await;

    let response = alice.create_task("t-empty-bob", "port the parser").await;
    let task_id: TaskId = response["result"]["task"]["id"]
        .as_str()
        .expect("the worker returned a Task with an id")
        .parse()
        .expect("the returned id is a valid task id");
    assert!(
        bob.wait_task_message(MSG_TIMEOUT).await,
        "bob saw the brief"
    );
    bob.task_checkin(
        &task_id,
        "accepted: port it",
        Progress::try_from(0).expect("0 is a percentage"),
    )
    .await;

    let error = bob
        .try_task_artifact(&task_id, "   ")
        .await
        .expect_err("a blank artifact must be refused");
    assert!(
        error
            .to_string()
            .contains("an artifact needs a text or a file"),
        "the refusal must say what is missing, got: {error}"
    );
    let task = served(
        &alice,
        "t-empty-bob",
        "GetTask",
        serde_json::json!({ "id": task_id.as_str() }),
    )
    .await;
    assert_eq!(
        task["status"]["state"], "TASK_STATE_WORKING",
        "the refused artifact must not park the task, got: {task}"
    );

    alice.leave().await;
    bob.leave().await;
}
