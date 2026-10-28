/// Whether `response` is the daemon's "nobody ever answered" transport timeout,
/// as opposed to a real reply carrying an error. Only the former is worth
/// re-issuing — see [`InProcNode::a2a_call`].
pub(crate) fn is_rpc_timeout(response: &serde_json::Value) -> bool {
    response["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("timed out"))
}
