/// Whether `response` is the daemon's "nobody ever answered" transport timeout,
/// as opposed to a real reply carrying an error. A call that opened a task answers with the task
/// marked `mesh:unconfirmed` instead of an error, and that is no answer either. Only these two are
/// worth re-issuing — see [`InProcNode::a2a_call`](crate::InProcNode::a2a_call).
pub(crate) fn is_rpc_timeout(response: &serde_json::Value) -> bool {
    let timed_out = response["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("timed out"));
    timed_out || response["result"]["task"]["metadata"]["mesh:unconfirmed"] == true
}
