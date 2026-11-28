/// Whether `response` is the daemon's "nobody ever answered" transport timeout,
/// as opposed to a real reply carrying an error. A call that opened a task answers with the task
/// marked `mesh:unconfirmed` instead of an error, and that is no answer either. Only these two are
/// worth re-issuing — see [`InProcNode::a2a_call`](crate::InProcNode::a2a_call).
pub(crate) fn is_rpc_timeout(response: &serde_json::Value) -> bool {
    let timed_out = response["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("timed out"));
    timed_out || unconfirmed_task_id(response).is_some()
}

/// The id of the task that `response` hands back unconfirmed, when it does. A caller that re-issues
/// the call cancels that task first, or the first attempt stays alive beside the retry.
pub(crate) fn unconfirmed_task_id(response: &serde_json::Value) -> Option<&str> {
    let task = &response["result"]["task"];
    (task["metadata"]["mesh:unconfirmed"] == true)
        .then(|| task["id"].as_str())
        .flatten()
}
