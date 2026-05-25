//! Integration tests: `ahs mcp` over stdio.
//!
//! Spawns the binary, pipes in JSON-RPC, asserts the server's
//! responses. These are the reliability guarantees we make at the
//! MCP surface.

mod common;

use ahs_shared::RATE_LIMIT_PER_MIN;
use common::{CONNECT_TIMEOUT, MSG_TIMEOUT};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::time::{Duration, Instant};

/// A UUID the server has never seen. Passing it as `after` lands
/// in the "evicted-id fallback" path, which returns the full
/// buffered log regardless of the implicit cursor — the only way
/// tests can inspect every message without disturbing cursor state.
const BOGUS_UUID: &str = "00000000-0000-0000-0000-000000000000";

/// Wrapper that spawns the binary, streams a conversation, and
/// collects responses keyed by id. Tests drive it synchronously.
struct McpClient {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
}

impl McpClient {
    fn spawn() -> Self {
        let mut child = common::test_cmd()
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn ahs mcp");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let reader = BufReader::new(stdout);
        let mut client = McpClient {
            child,
            stdin,
            reader,
        };
        // Complete the MCP handshake once so tests only worry about
        // tool-call shapes.
        client.send(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"0.0.1"}}}"#,
        );
        let _ = client.recv_until_response(1);
        client.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        client
    }

    fn send(&mut self, line: &str) {
        writeln!(self.stdin, "{line}").expect("write stdin");
    }

    /// Wait (bounded) for a JSON-RPC response with the given id.
    /// Returns the parsed value. Intermediate notifications are
    /// skipped.
    fn recv_until_response(&mut self, id: u64) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(
                Instant::now() <= deadline,
                "timed out waiting for response id={id}"
            );
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => panic!("server closed stdout while awaiting id={id}"),
                Ok(_) => {}
                Err(error) => panic!("stdout read error: {error}"),
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let parsed: serde_json::Value = serde_json::from_str(trimmed)
                .unwrap_or_else(|err| panic!("stdout line not JSON: {err}\nline: {trimmed}"));
            if parsed.get("id").and_then(serde_json::Value::as_u64) == Some(id) {
                return parsed;
            }
            // Otherwise it's a notification — skip.
        }
    }

    /// Simple tool call: send and return the matching response,
    /// skipping any notifications that arrive in the meantime.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "ergonomic test helper; callers pass json! literals by value"
    )]
    fn tool_call(&mut self, id: u64, name: &str, args: serde_json::Value) -> serde_json::Value {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": args }
        });
        self.send(&req.to_string());
        self.recv_until_response(id)
    }

    /// Shorthand for the `create_swarm` + extract-id ritual that
    /// opens almost every integration test. Returns
    /// `(swarm_id, nickname)`.
    fn create_and_get_swarm(&mut self, id: u64) -> (String, String) {
        let created = tool_result_json(&self.tool_call(
            id,
            "create_swarm",
            serde_json::json!({ "name": "mcptest" }),
        ))
        .expect("create_swarm must succeed");
        let swarm = created["swarm"]
            .as_str()
            .expect("create_swarm result must include swarm")
            .to_string();
        assert_eq!(
            created["name"].as_str(),
            Some("mcptest"),
            "create_swarm result must echo back the name"
        );
        let nickname = created["nickname"]
            .as_str()
            .expect("create_swarm result must include nickname")
            .to_string();
        (swarm, nickname)
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // All `tool_call`s are synchronous (send + read), so there
        // is nothing to wait for. Kill the child unconditionally
        // and reap it. Don't assert — we're in a Drop.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Helper: extract the JSON payload from a successful tool call
/// response. Returns None if the response is an error or the shape
/// is unexpected.
fn tool_result_json(response: &serde_json::Value) -> Option<serde_json::Value> {
    let content = response
        .get("result")?
        .get("content")?
        .as_array()?
        .first()?;
    let text = content.get("text")?.as_str()?;
    serde_json::from_str(text).ok()
}

fn tool_error(response: &serde_json::Value) -> Option<String> {
    // Either a JSON-RPC error (invalid args / not in swarm) or
    // `isError: true` in the result content.
    if let Some(msg) = response
        .get("error")
        .and_then(|error_obj| error_obj.get("message"))
        .and_then(|message| message.as_str())
    {
        return Some(msg.to_string());
    }
    if response
        .get("result")
        .and_then(|result| result.get("isError"))
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        return Some(
            response
                .get("result")
                .and_then(|result| result.get("content"))
                .and_then(|content| content.as_array())
                .and_then(|arr| arr.first())
                .and_then(|content| content.get("text"))
                .and_then(|text| text.as_str())
                .unwrap_or("unknown tool error")
                .to_string(),
        );
    }
    None
}

/// Spawn two MCP clients, have the first create a private swarm
/// and the second join it, then block until gossip has linked them
/// (creator sees the joiner's `joined` presence in its buffer).
/// Returns `(creator, joiner, swarm_id, creator_nickname)`.
///
/// Id reservations (so tests can't collide with our probes):
/// - `base_id + 0` — creator's `create_swarm`
/// - `base_id + 1` — joiner's `join_swarm`
/// - `base_id + 90_000 .. base_id + 90_050` — linkage-probe `fetch_messages`
///   on the creator. Tests must keep their own ids below that offset.
///
/// The probe polls every 100ms for up to 10s. This is deterministic
/// on both fast and loaded machines — no fixed fudge factor.
fn create_pair(base_id: u64) -> (McpClient, McpClient, String, String) {
    let mut creator = McpClient::spawn();
    let mut joiner = McpClient::spawn();
    let (swarm, creator_nick) = creator.create_and_get_swarm(base_id);
    tool_result_json(&joiner.tool_call(
        base_id + 1,
        "join_swarm",
        serde_json::json!({ "swarm": swarm.clone() }),
    ))
    .expect("join_swarm must succeed");

    // Poll the joiner's buffer until it contains a message authored
    // by the creator — the only unambiguous signal that iroh's
    // gossip has linked the two nodes. Once this lands, the
    // creator's initial `joined` (re-)announce has propagated and
    // any subsequent broadcast from the joiner will reach the
    // creator. Use BOGUS_UUID on `after` so the implicit cursor
    // doesn't hide buffered events (fetch_messages returns the full
    // buffer on unknown `after`) and also doesn't advance.
    // Suite-wide peer-link budget (shared with the other integration
    // binaries) — generous enough for threaded `cargo test` contention.
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut probe_id = base_id + 90_000;
    loop {
        let fetched = tool_result_json(&joiner.tool_call(
            probe_id,
            "fetch_messages",
            serde_json::json!({ "after": BOGUS_UUID }),
        ))
        .expect("linkage-probe fetch_messages must succeed");
        let linked = fetched["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|msg| {
                msg.get("author").and_then(|value| value.as_str()) == Some(creator_nick.as_str())
            });
        if linked {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "peers did not link within CONNECT_TIMEOUT; joiner buffer: {}",
            fetched["messages"]
        );
        probe_id += 1;
        std::thread::sleep(Duration::from_millis(100));
    }

    (creator, joiner, swarm, creator_nick)
}

// ─── stdout cleanliness ──────────────────────────────────────────

#[test]
fn mcp_stdout_is_pure_jsonrpc_through_full_lifecycle() {
    // Separate test that captures all stdout bytes and asserts every
    // non-empty line is JSON-RPC 2.0. Guards against any future
    // println! leak (the bug that motivated OutputMode::Silent).
    let mut child = common::test_cmd()
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let stdin = child.stdin.as_mut().unwrap();
    for line in [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"0.1"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"create_swarm","arguments":{"name":"mcptest"}}}"#,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"send_message","arguments":{"text":"sanity"}}}"#,
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"leave_swarm","arguments":{}}}"#,
    ] {
        writeln!(stdin, "{line}").unwrap();
    }
    std::thread::sleep(Duration::from_secs(8));
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait");
    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut responses = 0;
    for (i, line) in stdout.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|err| {
            panic!(
                "line {i} not JSON — stdout pollution.\n  error: {err}\n  line: {line}\n  full stdout:\n{stdout}"
            )
        });
        assert_eq!(
            parsed.get("jsonrpc").and_then(|value| value.as_str()),
            Some("2.0")
        );
        responses += 1;
    }
    assert!(
        responses >= 4,
        "expected >=4 JSON-RPC responses, got {responses}"
    );
}

// ─── idempotency + error paths ───────────────────────────────────

#[test]
fn tools_require_session_and_error_when_absent() {
    let mut client = McpClient::spawn();
    for (id, tool) in [
        (10, "send_message"),
        (11, "fetch_messages"),
        (12, "swarm_info"),
    ] {
        let args = if tool == "send_message" {
            serde_json::json!({ "text": "hi" })
        } else {
            serde_json::json!({})
        };
        let resp = client.tool_call(id, tool, args);
        let err = tool_error(&resp)
            .unwrap_or_else(|| panic!("{tool} without session should error, got: {resp}"));
        assert!(
            err.contains("not in a swarm"),
            "{tool}: expected 'not in a swarm' error, got: {err}"
        );
    }
}

#[test]
fn create_swarm_twice_errors_cleanly() {
    let mut client = McpClient::spawn();
    let first = client.tool_call(20, "create_swarm", serde_json::json!({ "name": "twice1" }));
    let first_json = tool_result_json(&first).expect("first create_swarm should succeed");
    assert!(first_json["swarm"].as_str().unwrap().starts_with("ahs"));
    assert_eq!(first_json["name"].as_str(), Some("twice1"));

    let second = client.tool_call(21, "create_swarm", serde_json::json!({ "name": "twice2" }));
    let err = tool_error(&second).expect("second create_swarm should error, but got success");
    assert!(
        err.contains("already in swarm"),
        "expected 'already in swarm' error, got: {err}"
    );
}

#[test]
fn join_swarm_idempotent_for_same_swarm() {
    let mut client = McpClient::spawn();
    let (swarm, nickname) = client.create_and_get_swarm(30);

    // Idempotent: join the swarm we just created with the same
    // nickname → no error, same handle back.
    let rejoin = client.tool_call(
        31,
        "join_swarm",
        serde_json::json!({ "swarm": swarm.clone(), "nickname": nickname.clone() }),
    );
    let rejoin_json = tool_result_json(&rejoin).unwrap_or_else(|| {
        panic!("idempotent join_swarm should succeed, but got error response: {rejoin}")
    });
    assert_eq!(rejoin_json["swarm"].as_str(), Some(swarm.as_str()));
    assert_eq!(rejoin_json["nickname"].as_str(), Some(nickname.as_str()));

    // Different nickname on the same swarm → error.
    let conflict = client.tool_call(
        32,
        "join_swarm",
        serde_json::json!({ "swarm": swarm, "nickname": "someone-else" }),
    );
    let err = tool_error(&conflict).expect("different-nick join should error");
    assert!(
        err.contains("already in swarm"),
        "expected 'already in swarm' error, got: {err}"
    );
}

#[test]
fn join_with_invalid_input_errors_gracefully() {
    let mut client = McpClient::spawn();
    let resp = client.tool_call(
        40,
        "join_swarm",
        serde_json::json!({ "swarm": "not-a-swarm-id" }),
    );
    let err = tool_error(&resp).expect("invalid join should error");
    // Exact wording can vary; just confirm we got an error and no
    // crash. Server must still accept the next request.
    assert!(!err.is_empty(), "error message must not be empty");

    // Prove the server is still alive: call a benign tool.
    let info = client.tool_call(41, "swarm_info", serde_json::json!({}));
    let err2 = tool_error(&info).expect("swarm_info with no session should error");
    assert!(err2.contains("not in a swarm"));
}

#[test]
fn leave_without_session_is_noop() {
    let mut client = McpClient::spawn();
    let resp = client.tool_call(50, "leave_swarm", serde_json::json!({}));
    // Docs say: "no-op if not in one." Must succeed.
    let json = tool_result_json(&resp).expect("leave with no session should succeed");
    assert_eq!(json["ok"], serde_json::json!(true));
}

#[test]
fn leave_then_create_cycle_works_within_one_server() {
    let mut client = McpClient::spawn();
    let first = tool_result_json(&client.tool_call(
        60,
        "create_swarm",
        serde_json::json!({ "name": "cycle1" }),
    ))
    .expect("create");
    let first_swarm = first["swarm"].as_str().unwrap().to_string();

    let _ = client.tool_call(61, "leave_swarm", serde_json::json!({}));

    let second = tool_result_json(&client.tool_call(
        62,
        "create_swarm",
        serde_json::json!({ "name": "cycle2" }),
    ))
    .expect("create after leave");
    assert_ne!(
        second["swarm"].as_str().unwrap(),
        first_swarm,
        "second create should mint a fresh swarm id"
    );
}

#[test]
fn send_message_without_session_errors() {
    let mut client = McpClient::spawn();
    let resp = client.tool_call(70, "send_message", serde_json::json!({ "text": "orphan" }));
    let err = tool_error(&resp).expect("send_message without session should error");
    assert!(err.contains("not in a swarm"));
}

#[test]
fn create_swarm_with_granular_relay_succeeds() {
    // Granular lookups: naming `relay` opts into it directly, the same way
    // the CLI `--relay` flag does — the old "relay requires public" rule is
    // gone, so a relay-only (network:private) swarm now creates fine.
    let mut client = McpClient::spawn();
    let resp = client.tool_call(
        80,
        "create_swarm",
        serde_json::json!({ "name": "relayp", "network": "private", "relay": "https://relay.example/" }),
    );
    let result = tool_result_json(&resp).expect("granular relay create should succeed");
    assert!(
        result["swarm"]
            .as_str()
            .unwrap_or_default()
            .starts_with("ahs"),
        "expected a swarm id, got: {result}"
    );
}

#[test]
fn create_swarm_with_unknown_network_errors() {
    let mut client = McpClient::spawn();
    let resp = client.tool_call(
        90,
        "create_swarm",
        serde_json::json!({ "name": "bogus1", "network": "bogus" }),
    );
    let err = tool_error(&resp).expect("bogus network should error");
    // The `network` arg is a typed enum, so an unknown value is rejected
    // at parameter deserialization with the valid set named.
    assert!(
        err.contains("private") && err.contains("public"),
        "expected the error to name the valid network modes, got: {err}"
    );
}

#[test]
fn send_empty_body_works() {
    // Protocol doesn't forbid empty body; just make sure it doesn't
    // crash and the echo / buffer both reflect it.
    let mut client = McpClient::spawn();
    let (_swarm, _) = client.create_and_get_swarm(100);

    let sent = client.tool_call(101, "send_message", serde_json::json!({ "text": "" }));
    let sent_json = tool_result_json(&sent).expect("empty-body send should succeed");
    let id = sent_json["id"].as_str().unwrap().to_string();
    assert!(!id.is_empty());
    // send_message now returns the full echo inline — the agent
    // should never need a follow-up fetch for its own send.
    let echo = sent_json
        .get("message")
        .expect("send_message must return an echo");
    assert_eq!(echo["id"].as_str(), Some(id.as_str()));
    assert_eq!(echo["body"].as_str(), Some(""));
    assert!(echo["ts"].is_i64());

    // BOGUS_UUID dodges the implicit cursor to inspect the buffer.
    let fetched = tool_result_json(&client.tool_call(
        102,
        "fetch_messages",
        serde_json::json!({ "after": BOGUS_UUID }),
    ))
    .expect("fetch");
    let msgs = fetched["messages"].as_array().unwrap();
    assert!(
        msgs.iter().any(|msg| msg["id"].as_str() == Some(&id)),
        "buffer should contain the just-sent message, got {msgs:?}"
    );
}

#[test]
fn fetch_messages_with_unknown_after_returns_buffer() {
    // Documented behavior: if `after` isn't in the buffer, poll
    // returns all buffered messages (with an info event in JSON
    // mode, which is suppressed in Silent). Must not crash.
    let mut client = McpClient::spawn();
    client.create_and_get_swarm(110);
    let _ = client.tool_call(111, "send_message", serde_json::json!({ "text": "a" }));

    let fetched = tool_result_json(&client.tool_call(
        112,
        "fetch_messages",
        serde_json::json!({ "after": BOGUS_UUID }),
    ))
    .expect("fetch with unknown after");
    assert!(fetched["messages"].is_array());
}

#[test]
fn reply_to_unknown_nickname_still_succeeds() {
    // We don't validate the reply target nickname at the protocol
    // level — the message is broadcast, peers can still receive it.
    // Just confirm the server doesn't panic.
    let mut client = McpClient::spawn();
    client.create_and_get_swarm(120);
    let resp = client.tool_call(
        121,
        "send_message",
        serde_json::json!({
            "text": "orphan reply",
            "reply": "no-such-peer"
        }),
    );
    let json = tool_result_json(&resp).expect("reply to unknown nick should succeed");
    assert!(!json["id"].as_str().unwrap().is_empty());
}

// ─── cursor pagination (#1) ──────────────────────────────────────

#[test]
fn fetch_messages_cursor_returns_only_new_since_last_call() {
    // The server auto-tracks an implicit cursor across every tool
    // call: `send_message` advances it past the sent id, and
    // `fetch_messages` advances it past the last returned id.
    // So cursor-less fetches after a burst of self-sends return
    // nothing new — the agent already has the echo from each
    // `send_message` return.
    let mut client = McpClient::spawn();
    client.create_and_get_swarm(200);

    // Send three messages, capturing the first id so we can later
    // replay from before `send_message` advanced the cursor.
    let first = tool_result_json(&client.tool_call(
        201,
        "send_message",
        serde_json::json!({ "text": "one" }),
    ))
    .expect("send ok");
    let first_send_id = first["id"].as_str().unwrap().to_string();
    for (i, body) in ["two", "three"].iter().enumerate() {
        client.tool_call(
            202 + i as u64,
            "send_message",
            serde_json::json!({ "text": body }),
        );
    }

    // Cursor-less fetch: implicit cursor is now past all three
    // self-sends, so delta is empty.
    let initial_fetch =
        tool_result_json(&client.tool_call(210, "fetch_messages", serde_json::json!({})))
            .expect("initial fetch");
    let msgs1 = initial_fetch["messages"]
        .as_array()
        .expect("messages array");
    assert!(
        msgs1.is_empty(),
        "implicit cursor should be past all self-sends, got {msgs1:?}"
    );

    // Explicit `after` overrides the implicit cursor — replay
    // from after the first send yields the remaining two.
    let replay = tool_result_json(&client.tool_call(
        211,
        "fetch_messages",
        serde_json::json!({ "after": first_send_id }),
    ))
    .expect("explicit replay");
    let replay_msgs = replay["messages"].as_array().unwrap();
    assert_eq!(
        replay_msgs.len(),
        2,
        "explicit after must override implicit cursor, expected 2 got {replay_msgs:?}"
    );
    let cursor = replay["current_id"]
        .as_str()
        .expect("current_id should be a string when batch non-empty")
        .to_string();
    assert_eq!(
        replay_msgs.last().unwrap()["id"].as_str(),
        Some(cursor.as_str()),
        "current_id must match id of last message in batch"
    );

    // Send one more, then fetch with the cursor. Only the new message.
    let _ = client.tool_call(220, "send_message", serde_json::json!({ "text": "four" }));
    let second = tool_result_json(&client.tool_call(
        221,
        "fetch_messages",
        serde_json::json!({ "after": cursor }),
    ))
    .expect("delta fetch");
    let msgs2 = second["messages"].as_array().unwrap();
    assert_eq!(
        msgs2.len(),
        1,
        "delta fetch should return only the new msg, got {}",
        msgs2.len()
    );
    assert_eq!(msgs2[0]["body"].as_str(), Some("four"));
    let cursor2 = second["current_id"].as_str().unwrap().to_string();
    assert_ne!(
        cursor2, cursor,
        "current_id must advance past the old cursor"
    );

    // Fetch again with the new cursor — nothing newer, empty batch,
    // null current_id.
    let third = tool_result_json(&client.tool_call(
        222,
        "fetch_messages",
        serde_json::json!({ "after": cursor2 }),
    ))
    .expect("idle fetch");
    let msgs3 = third["messages"].as_array().unwrap();
    assert!(
        msgs3.is_empty(),
        "fetch with up-to-date cursor should be empty, got {msgs3:?}"
    );
    assert!(
        third["current_id"].is_null(),
        "empty batch → null current_id, got: {}",
        third["current_id"]
    );
}

// ─── rate limiting (#6) ──────────────────────────────────────────

#[test]
fn rate_limiter_drops_excess_messages_from_flooding_peer() {
    // Sender creates a private swarm, receiver joins, gossip links.
    let (mut sender, mut receiver, _swarm, sender_nick) = create_pair(300);

    // Sender fires many messages in tight succession — twice the
    // per-identity quota, so excess is dropped regardless of token
    // refills during the loop. With sender-side limiting the sender
    // drops most of these before broadcast (the receiver's limiter is
    // the backstop); either way the flood is bounded well under the
    // count sent. Referencing the constant keeps this from silently
    // passing if the quota changes.
    let message_flood: usize = RATE_LIMIT_PER_MIN as usize * 2;
    for i in 0..message_flood {
        let _ = sender.tool_call(
            400 + i as u64,
            "send_message",
            serde_json::json!({ "text": format!("flood {}", i) }),
        );
    }

    // Poll fetch_messages until the count stops growing (gossip has
    // fully caught up). BOGUS_UUID keeps the implicit cursor out of
    // the way so we always see the full buffer. Suite-wide gossip-
    // delivery budget (breaks early once the count stabilises).
    let deadline = Instant::now() + MSG_TIMEOUT;
    let mut last_count: i64 = -1;
    let mut stable_iters = 0;
    let mut flood_count: usize = 0;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(400));
        let fetched = tool_result_json(&receiver.tool_call(
            500,
            "fetch_messages",
            serde_json::json!({ "after": BOGUS_UUID }),
        ))
        .expect("fetch");
        let msgs = fetched["messages"].as_array().expect("messages array");
        flood_count = msgs
            .iter()
            .filter(|msg| {
                msg.get("type").and_then(|value| value.as_str()) == Some("msg")
                    && msg.get("author").and_then(|value| value.as_str())
                        == Some(sender_nick.as_str())
                    && msg
                        .get("body")
                        .and_then(|value| value.as_str())
                        .is_some_and(|body| body.starts_with("flood "))
            })
            .count();
        if i64::try_from(flood_count).expect("flood_count fits i64") == last_count {
            stable_iters += 1;
            if stable_iters >= 3 {
                break;
            }
        } else {
            stable_iters = 0;
            last_count = i64::try_from(flood_count).expect("flood_count fits i64");
        }
    }

    // The rate limiter should have dropped at least some messages.
    // We don't pin the exact count — token-bucket behavior is
    // timing-sensitive — just that it fired at all.
    assert!(
        flood_count < message_flood,
        "rate limiter should drop at least one flood message; receiver got all {flood_count} of {message_flood}"
    );
    // And the receiver should have seen at least the burst allowance.
    assert!(
        flood_count >= 1,
        "receiver should see at least one message through the rate limiter, got 0"
    );
}
