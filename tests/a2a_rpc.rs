//! Gossip A2A request/response (directed peer RPC), in-process over the real
//! event loop + iroh mesh (`InProcNode`): a peer calls another peer's A2A
//! server over gossip and awaits its response. Covers the safe method set
//! (a read, a `message/send` that opens a task and returns it), the safe-set
//! rejection of a mutating op, and the fast-fail for an unknown peer.

mod common;

use std::time::Duration;

use agent_square::embed::A2aCallParams;
use common::{InProcNode, MSG_TIMEOUT};
use serde_json::json;

const RPC_TIMEOUT: Duration = MSG_TIMEOUT;

/// A `tasks/list` call to a peer returns *that peer's* task view over gossip.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_tasks_list_reaches_the_peer() {
    let mut alice = InProcNode::create("rpc-list").await;
    let mut bob = InProcNode::join(&alice.mesh, "rpc-bob").await;
    alice.send("link").await;
    assert!(bob.wait_body("link", MSG_TIMEOUT).await, "bob meshed");
    // Prove the reverse path too: the RPC response rides bob's outbound, and
    // A2aReq/A2aResp are not loggable (no anti-entropy healing), so a reply
    // broadcast into a still-converging overlay is lost for good and the call
    // dies as a 60s waiter timeout — the suite's long-standing flake.
    bob.send("link-back").await;
    assert!(
        alice.wait_body("link-back", MSG_TIMEOUT).await,
        "alice meshed"
    );

    let response = alice
        .session
        .a2a_call(A2aCallParams {
            peer: "rpc-bob".parse().unwrap(),
            method: "ListTasks".to_string(),
            params: json!({}),
            timeout: RPC_TIMEOUT,
        })
        .await
        .expect("a2a_call returns");
    assert!(
        response["result"]["tasks"].is_array(),
        "ListTasks result carries a tasks array: {response}"
    );

    alice.leave().await;
    bob.leave().await;
}

/// A directed `message/send` (no `taskId`) over gossip creates a task: the
/// peer (the A2A server) **mints a fresh server id**, opens the task, and
/// returns the authoritative `submitted` `Task` synchronously. No
/// deterministic id, no `mesh-task` marker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_message_send_opens_task_and_returns_it() {
    let mut alice = InProcNode::create("rpc-send").await;
    let mut bob = InProcNode::join(&alice.mesh, "rpc-send-bob").await;
    alice.send("link").await;
    assert!(bob.wait_body("link", MSG_TIMEOUT).await, "bob meshed");
    // Prove the reverse path too: the RPC response rides bob's outbound, and
    // A2aReq/A2aResp are not loggable (no anti-entropy healing), so a reply
    // broadcast into a still-converging overlay is lost for good and the call
    // dies as a 60s waiter timeout — the suite's long-standing flake.
    bob.send("link-back").await;
    assert!(
        alice.wait_body("link-back", MSG_TIMEOUT).await,
        "alice meshed"
    );

    let our_message_id = "550e8400-e29b-41d4-a716-446655440000";
    let create = json!({ "message": {
        "messageId": our_message_id,
        "role": "ROLE_USER",
        "parts": [{ "text": "review src/net" }],
        "contextId": alice.mesh,
    }});
    let response = alice
        .session
        .a2a_call(A2aCallParams {
            peer: "rpc-send-bob".parse().unwrap(),
            method: "SendMessage".to_string(),
            params: create,
            timeout: RPC_TIMEOUT,
        })
        .await
        .expect("a2a_call returns");
    // v1.0 SendMessage returns a SendMessageResponse oneof: {"task": <Task>}.
    let task = &response["result"]["task"];
    assert_eq!(
        task["status"]["state"], "TASK_STATE_SUBMITTED",
        "got: {response}"
    );
    // The task id is server-minted — a fresh uuid, distinct from our message id.
    let task_id = task["id"].as_str().expect("task id");
    assert_ne!(
        task_id, our_message_id,
        "the worker mints its own id, not our messageId"
    );

    alice.leave().await;
    bob.leave().await;
}

/// A mutating op (`mesh/state.merge`) is refused by the safe-set gate — a
/// peer never authors global state on the caller's behalf.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_state_merge_is_refused() {
    let mut alice = InProcNode::create("rpc-merge").await;
    let mut bob = InProcNode::join(&alice.mesh, "rpc-merge-bob").await;
    alice.send("link").await;
    assert!(bob.wait_body("link", MSG_TIMEOUT).await, "bob meshed");
    // Prove the reverse path too: the RPC response rides bob's outbound, and
    // A2aReq/A2aResp are not loggable (no anti-entropy healing), so a reply
    // broadcast into a still-converging overlay is lost for good and the call
    // dies as a 60s waiter timeout — the suite's long-standing flake.
    bob.send("link-back").await;
    assert!(
        alice.wait_body("link-back", MSG_TIMEOUT).await,
        "alice meshed"
    );

    let response = alice
        .session
        .a2a_call(A2aCallParams {
            peer: "rpc-merge-bob".parse().unwrap(),
            method: "mesh/state.merge".to_string(),
            params: json!({ "merge": { "x": 1 } }),
            timeout: RPC_TIMEOUT,
        })
        .await
        .expect("a2a_call returns");
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("not permitted")),
        "state.merge over gossip must be refused: {response}"
    );

    alice.leave().await;
    bob.leave().await;
}

/// Creating a task on a peer that isn't a current participant fails fast (no
/// wait for the timeout). The hard roster check gates task *creation* only;
/// follow-ups/reads to a briefly-absent peer are let through to the timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_unknown_peer_fails_fast() {
    let alice = InProcNode::create("rpc-unknown").await;

    let create = json!({ "message": {
        "messageId": "550e8400-e29b-41d4-a716-446655440000",
        "role": "ROLE_USER",
        "parts": [{ "text": "hi" }],
    }});
    let response = alice
        .session
        .a2a_call(A2aCallParams {
            peer: "ghost-peer".parse().unwrap(),
            method: "SendMessage".to_string(),
            params: create,
            timeout: RPC_TIMEOUT,
        })
        .await
        .expect("a2a_call returns");
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("unknown participant")),
        "creating a task on a non-participant fails fast: {response}"
    );

    alice.leave().await;
}
