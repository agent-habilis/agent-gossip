use agent_gossip::TaskId;

/// The `Task` id of a `SendMessage` response (v1.0 `SendMessageResponse` oneof:
/// `{"result":{"task":<Task>}}`).
///
/// # Panics
///
/// Panics if the response carries no task id, or if the id is not a valid task id.
#[must_use]
pub fn task_id_of(response: &serde_json::Value) -> TaskId {
    response["result"]["task"]["id"]
        .as_str()
        .expect("the worker returned a Task with an id")
        .parse()
        .expect("the returned id is a valid task id")
}
