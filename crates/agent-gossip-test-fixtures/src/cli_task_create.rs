use std::time::Instant;

use crate::{
    MSG_TIMEOUT, await_peer_card_cli, cli_task_create_attempt, is_rpc_timeout, test_cmd,
    unconfirmed_task_id,
};

/// Create a task on a **known** peer and return its task id
/// (parsed from the JSON-RPC response the call prints on stdout). Panics on
/// failure.
///
/// Waits for `to`'s card first: a directed call is rejected until the seal key
/// replicates, so issuing one the moment the mesh links is a race. See
/// [`await_peer_card_cli`](fn@await_peer_card_cli).
///
/// Then **re-issues while the peer never answers**, on the same reasoning as
/// [`InProcNode::a2a_call_retrying`](crate::InProcNode::a2a_call_retrying) — the card arriving does not mean the
/// overlay holds a live link, and a directed request sent into that gap is
/// dropped for good rather than healed, leaving the sender to block until the
/// heal tick re-bridges the pair. Waiting on the card guards one race; this
/// guards the other. Only the transport timeout is retried: a real answer,
/// including a semantic error like `unknown peer`, means the round trip
/// completed and is handed to the assertion below untouched.
///
/// # Panics
///
/// Panics if the call fails, or if its response carries no task id.
#[must_use]
pub fn cli_task_create(mesh: &str, nickname: &str, to: &str, text: &str) -> String {
    await_peer_card_cli(mesh, nickname, to);
    let deadline = Instant::now() + MSG_TIMEOUT;
    let out = loop {
        let attempt = cli_task_create_attempt(mesh, nickname, to, text);
        let body: serde_json::Value =
            serde_json::from_slice(&attempt.stdout).unwrap_or(serde_json::Value::Null);
        if !is_rpc_timeout(&body) || Instant::now() >= deadline {
            break attempt;
        }
        // The task that the attempt left unconfirmed must not outlive the retry. The worker may
        // not hold it, and then the cancel is refused, which is fine.
        if let Some(task_id) = unconfirmed_task_id(&body) {
            cancel_attempt(mesh, nickname, to, task_id);
        }
    };
    // stdout, not just stderr: a rejected JSON-RPC call reports itself in the
    // response body it prints on stdout, so a stderr-only message renders as a
    // bare "a2a create failed:" — which is what CI failures looked like.
    assert!(
        out.status.success(),
        "a2a create failed ({}) from <{nickname}> to <{to}>\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout).trim(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("a2a call prints a JSON-RPC response");
    parsed["result"]["task"]["id"]
        .as_str()
        .expect("the call returned a Task id")
        .to_string()
}

/// Cancel the task that a call left unconfirmed, so that the call can be re-issued.
fn cancel_attempt(mesh: &str, nickname: &str, to: &str, task_id: &str) {
    let params = serde_json::json!({ "id": task_id }).to_string();
    let _ = test_cmd()
        .args(["a2a", "call", "--gossip", mesh, "--nickname", nickname])
        .args(["--to", to, "--method", "CancelTask", "--params", &params])
        .output();
}
