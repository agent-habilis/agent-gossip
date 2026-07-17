//! Wire contract for the `--a2a-serve` localhost JSON-RPC 2.0 binding:
//! subprocess daemons, raw HTTP/1.1 over `std::net::TcpStream` (no client
//! crate — the bytes are the contract). Covers discovery (state-file
//! `a2a_port`/`a2a_token`, the unauthenticated well-known card), auth
//! (bearer required on every RPC), broadcast `message/send` (chat), a
//! conformant + reachable peer card, directed `message/send` (native task
//! creation, routed through the gossip request/response waiter — an
//! off-the-shelf client can delegate a task), `tasks/get`/`tasks/list`, the
//! `mesh-state` extension methods, and the cross-binding property: a JSON-RPC
//! send on one node surfaces on a plain gossip peer.

use agent_gossip_test_fixtures as common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Instant;

use common::{CONNECT_TIMEOUT, MSG_TIMEOUT, Node, POLL, wait_until};
use serde_json::Value;

/// One-shot HTTP/1.1 request against the local binding; returns
/// `(status, body)`.
fn http(port: u16, request: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to a2a port");
    stream.write_all(request.as_bytes()).expect("write request");
    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("read response");
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("status code");
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    (status, body)
}

fn get(port: u16, path: &str) -> (u16, String) {
    http(
        port,
        &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"),
    )
}

fn post(port: u16, path: &str, token: &str, body: &Value) -> (u16, Value) {
    let payload = body.to_string();
    let (status, raw) = http(
        port,
        &format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        ),
    );
    let parsed = serde_json::from_str(&raw).unwrap_or(Value::Null);
    (status, parsed)
}

/// One JSON-RPC call's identity + payload — grouped so the [`rpc`] test helper
/// stays within the argument budget.
#[derive(Clone, Copy)]
struct RpcCall<'a> {
    port: u16,
    path: &'a str,
    token: &'a str,
    method: &'a str,
    params: &'a Value,
}

/// A JSON-RPC call that must succeed; returns `result`.
fn rpc(call: RpcCall<'_>) -> Value {
    let RpcCall {
        port,
        path,
        token,
        method,
        params,
    } = call;
    let (status, response) = post(
        port,
        path,
        token,
        &serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params.clone() }),
    );
    assert_eq!(status, 200, "rpc {method} transport failure: {response}");
    assert!(
        response["error"].is_null(),
        "rpc {method} errored: {}",
        response["error"]
    );
    response["result"].clone()
}

/// Poll the session state file until it reports `ready` with a bound A2A
/// endpoint; returns `(port, token)`.
fn wait_a2a(state_file: &std::path::Path) -> (u16, String) {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    while Instant::now() < deadline {
        if let Ok(raw) = std::fs::read_to_string(state_file)
            && let Ok(parsed) = serde_json::from_str::<Value>(&raw)
            && parsed["ready"] == true
            && let (Some(port), Some(token)) =
                (parsed["a2a_port"].as_u64(), parsed["a2a_token"].as_str())
        {
            return (
                u16::try_from(port).expect("port fits u16"),
                token.to_string(),
            );
        }
        std::thread::sleep(POLL);
    }
    panic!(
        "daemon never reported a bound a2a endpoint in {}",
        state_file.display()
    );
}

/// The whole binding contract against one live pair (amortizes the daemon
/// startup): discovery → card → auth → chat send (cross-binding delivery to
/// a plain gossip peer) → peer card → task creation → tasks/get + tasks/list →
/// the mesh-state extension.
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end binding-contract test; spawning a daemon pair is expensive, so every localhost-binding assertion shares the one live pair"
)]
#[test]
fn a2a_binding_serves_card_rpc_and_tasks() {
    let state_file = std::env::temp_dir().join(format!(
        "agent-gossip-a2a-contract-{}.state.json",
        std::process::id()
    ));
    let state_str = state_file.to_string_lossy().to_string();
    let (host, mesh) =
        Node::create_args("a2atest", &["--a2a-serve", "--state-file", &state_str], &[]);
    let joiner = Node::join(&mesh, "a2a-peer");
    assert!(host.wait_ready(&mesh), "host socket never appeared");
    assert!(joiner.wait_ready(&mesh), "joiner socket never appeared");
    let (port, token) = wait_a2a(&state_file);

    // The card is served unauthenticated and self-describes the binding.
    let (status, card) = get(port, "/.well-known/agent-card.json");
    assert_eq!(status, 200, "well-known card must not require auth");
    let card: Value = serde_json::from_str(&card).expect("card is JSON");
    assert_eq!(card["name"].as_str(), Some(host.nickname.as_str()));
    assert_eq!(card["capabilities"]["streaming"], true);
    // A2A v1.0: reachable endpoints live in `supportedInterfaces`, each with its
    // own `protocolVersion`. The served card carries the JSONRPC interface (the
    // bound port) alongside the always-present gossip one.
    let interfaces = card["supportedInterfaces"]
        .as_array()
        .expect("supportedInterfaces array");
    assert!(
        interfaces
            .iter()
            .any(|iface| iface["protocolBinding"] == "JSONRPC"
                && iface["url"]
                    .as_str()
                    .is_some_and(|url| url.contains(&port.to_string()))
                && iface["protocolVersion"] == "1.0"),
        "the card advertises the bound JSONRPC endpoint: {card}"
    );

    // Every RPC requires the bearer token.
    let (unauth_status, _) = post(
        port,
        "/mesh",
        "not-the-token",
        &serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "ListTasks", "params": {} }),
    );
    assert_eq!(unauth_status, 401, "a wrong bearer token must be rejected");

    // message/send on the mesh endpoint broadcasts; the echo is the
    // daemon-authored A2A Message.
    let echo = rpc(RpcCall {
        port,
        path: "/mesh",
        token: &token,
        method: "SendMessage",
        params: &serde_json::json!({ "message": {
            "messageId": "650e8400-e29b-41d4-a716-446655440111",
            "role": "ROLE_USER",
            "parts": [{ "text": "hello from a2a" }],
        }}),
    });
    // v1.0 SendMessageResponse oneof: a broadcast has no task, so `{"message":…}`.
    assert_eq!(echo["message"]["parts"][0]["text"], "hello from a2a");
    // Cross-binding: the plain gossip peer surfaces the text.
    let delivered = wait_until(
        || joiner.count_from(&host.nickname, "hello from a2a"),
        1,
        MSG_TIMEOUT,
    );
    assert!(
        delivered >= 1,
        "a JSON-RPC send never reached the gossip peer"
    );

    // A peer's card, served over this binding, is A2A-conformant and points at
    // our relaying endpoint (we relay JSON-RPC to that gossip-only peer).
    let (peer_card_status, peer_card) = get(port, "/peers/a2a-peer/.well-known/agent-card.json");
    assert_eq!(peer_card_status, 200, "peer card must be served");
    let peer_card: Value = serde_json::from_str(&peer_card).expect("peer card is JSON");
    assert!(
        peer_card["supportedInterfaces"]
            .as_array()
            .is_some_and(|ifaces| ifaces
                .iter()
                .any(|iface| iface["protocolBinding"] == "JSONRPC"
                    && iface["url"]
                        .as_str()
                        .is_some_and(|url| url.contains("/peers/a2a-peer")))),
        "peer card advertises the relaying JSONRPC interface: {peer_card}"
    );

    // Native task creation: a directed `message/send` to the peer is routed
    // through the gossip request/response waiter — the peer mints the task id
    // and returns the Task synchronously (an off-the-shelf A2A client can
    // delegate a task over the compliant transport).
    let task = rpc(RpcCall {
        port,
        path: "/peers/a2a-peer",
        token: &token,
        method: "SendMessage",
        params: &serde_json::json!({ "message": {
            "messageId": "550e8400-e29b-41d4-a716-446655440000",
            "role": "ROLE_USER",
            "parts": [{ "text": "review src/net" }],
        }}),
    });
    // v1.0 SendMessageResponse oneof: creation yields `{"task": <Task>}`.
    assert_eq!(
        task["task"]["status"]["state"], "TASK_STATE_SUBMITTED",
        "creation returns a Task: {task}"
    );
    let task_id = task["task"]["id"]
        .as_str()
        .expect("server-minted task id")
        .to_string();

    // The initiator (this host) adopted the task; tasks/get + tasks/list see it.
    let fetched = rpc(RpcCall {
        port,
        path: "/mesh",
        token: &token,
        method: "GetTask",
        params: &serde_json::json!({ "id": task_id }),
    });
    assert_eq!(fetched["id"].as_str(), Some(task_id.as_str()));
    let listed = rpc(RpcCall {
        port,
        path: "/mesh",
        token: &token,
        method: "ListTasks",
        params: &serde_json::json!({}),
    });
    assert!(
        listed["tasks"]
            .as_array()
            .is_some_and(|tasks| tasks.iter().any(|entry| entry["id"] == task_id.as_str())),
        "tasks/list must include the created task: {listed}"
    );

    // The mesh-state extension methods mirror `agent-gossip state merge/get`.
    rpc(RpcCall {
        port,
        path: "/mesh",
        token: &token,
        method: "mesh/state.merge",
        params: &serde_json::json!({ "merge": { "turn": "a2a" } }),
    });
    let doc = rpc(RpcCall {
        port,
        path: "/mesh",
        token: &token,
        method: "mesh/state.get",
        params: &serde_json::json!({}),
    });
    assert_eq!(doc["turn"], "a2a");

    // GetExtendedAgentCard (authenticated) returns the full v1.0 card.
    let extended = rpc(RpcCall {
        port,
        path: "/mesh",
        token: &token,
        method: "GetExtendedAgentCard",
        params: &Value::Null,
    });
    assert!(
        extended["supportedInterfaces"].is_array(),
        "extended card is a v1.0 AgentCard: {extended}"
    );
    assert_eq!(extended["capabilities"]["extendedAgentCard"], true);

    // Push-notification config methods are intentionally unsupported: the
    // JSON-RPC method-not-found error, and `capabilities.pushNotifications`
    // is declared false on the card.
    assert_eq!(card["capabilities"]["pushNotifications"], false);
    let (push_status, push_response) = post(
        port,
        "/mesh",
        &token,
        &serde_json::json!({ "jsonrpc": "2.0", "id": 9, "method": "CreateTaskPushNotificationConfig", "params": {} }),
    );
    assert_eq!(push_status, 200);
    assert_eq!(push_response["error"]["code"], -32601);

    let _ = std::fs::remove_file(&state_file);
}

/// Without `--a2a-serve` the binding is off: the state file reports no
/// endpoint and nothing listens.
#[test]
fn a2a_binding_is_off_by_default() {
    let state_file = std::env::temp_dir().join(format!(
        "agent-gossip-a2a-default-{}.state.json",
        std::process::id()
    ));
    let state_str = state_file.to_string_lossy().to_string();
    let (host, mesh) = Node::create_args("a2aoff", &["--state-file", &state_str], &[]);
    assert!(host.wait_ready(&mesh), "host socket never appeared");
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let parsed = loop {
        if let Ok(raw) = std::fs::read_to_string(&state_file)
            && let Ok(parsed) = serde_json::from_str::<Value>(&raw)
            && parsed["ready"] == true
        {
            break parsed;
        }
        assert!(Instant::now() < deadline, "state file never reported ready");
        std::thread::sleep(POLL);
    };
    assert!(
        parsed.get("a2a_port").is_none() && parsed.get("a2a_token").is_none(),
        "the binding must be off by default: {parsed}"
    );
    let _ = std::fs::remove_file(&state_file);
}
