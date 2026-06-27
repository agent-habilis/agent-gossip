//! Integration tests: `ahsw mcp` over stdio.
//!
//! Spawns the binary, pipes in JSON-RPC, asserts the server's
//! responses. These are the reliability guarantees we make at the
//! MCP surface.

mod common;

use agent_habilis_swarm::RATE_LIMIT_PER_MIN;
use common::{CONNECT_TIMEOUT, MSG_TIMEOUT, POLL, flag_args, test_cmd, tmp_log};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::time::{Duration, Instant};

/// The before-anything seq cursor. Passing `0` as `after` returns the full
/// buffered log and, being an *explicit* override, never advances the
/// session's implicit cursor — the only way tests can inspect every event
/// without disturbing cursor state.
const FROM_START: u64 = 0;

/// Wrapper that spawns the binary, streams a conversation, and
/// collects responses keyed by id. Tests drive it synchronously.
struct McpClient {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
}

impl McpClient {
    fn spawn() -> Self {
        Self::spawn_with_args(&[])
    }

    /// Spawn the MCP server without performing the handshake — for tests that
    /// assert on the `initialize` response itself.
    fn spawn_raw() -> Self {
        let mut child = test_cmd()
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn ahsw mcp");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let reader = BufReader::new(stdout);
        McpClient {
            child,
            stdin,
            reader,
        }
    }

    /// Spawn the MCP server with extra `mcp`-subcommand flags (e.g. the hidden
    /// `--directory-private` so the directory path runs on the loopback ladder).
    fn spawn_with_args(extra: &[&str]) -> Self {
        let mut child = test_cmd()
            .arg("mcp")
            .args(extra)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn ahsw mcp");
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
    serde_json::from_str(&tool_result_text(response)?).ok()
}

fn tool_result_text(response: &serde_json::Value) -> Option<String> {
    let content = response
        .get("result")?
        .get("content")?
        .as_array()?
        .first()?;
    Some(content.get("text")?.as_str()?.to_string())
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
    create_pair_with(base_id, &[])
}

/// Like [`create_pair`] but spawns the creator with extra `mcp` flags (e.g. a
/// short `--ping-window-secs` so a `ping` round finalizes fast).
fn create_pair_with(base_id: u64, creator_args: &[&str]) -> (McpClient, McpClient, String, String) {
    let mut creator = McpClient::spawn_with_args(creator_args);
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
    // creator. Use FROM_START (seq 0) on `after` so the implicit cursor
    // doesn't hide buffered events (fetch_messages returns the full
    // buffer) and, being explicit, doesn't advance the implicit cursor.
    // Suite-wide peer-link budget (shared with the other integration
    // binaries) — generous enough for threaded `cargo test` contention.
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut probe_id = base_id + 90_000;
    loop {
        let fetched = tool_result_json(&joiner.tool_call(
            probe_id,
            "fetch_messages",
            serde_json::json!({ "after": FROM_START }),
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
    let mut child = test_cmd()
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
fn swarm_version_works_without_a_session() {
    let mut client = McpClient::spawn();
    // Unlike the messaging tools, version is a local check — no swarm needed.
    let resp = client.tool_call(30, "swarm_version", serde_json::json!({}));
    let json = tool_result_json(&resp).expect("swarm_version should return a JSON result");
    assert!(
        json["version"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "swarm_version must report a non-empty build string, got: {json}"
    );
    // MCP carries no skill of its own, so the version result is version-only —
    // the former skill-drift fields are gone.
    assert!(
        json.get("skill_up_to_date").is_none() && json.get("skill_state").is_none(),
        "swarm_version must not report skill fields over MCP, got: {json}"
    );
}

#[test]
fn swarm_manual_returns_the_manual_without_a_session() {
    let mut client = McpClient::spawn();
    let resp = client.tool_call(31, "swarm_manual", serde_json::json!({}));
    let text = tool_result_text(&resp).expect("swarm_manual should return text content");
    assert!(
        text.contains("COMMANDS") && text.contains("JSON EVENTS"),
        "swarm_manual must return the full manual, got {} chars",
        text.len()
    );
}

#[test]
fn initialize_carries_behavioral_instructions() {
    // The server's `instructions` is the MCP-half of the old generic skill:
    // a capable client surfaces it at handshake, so MCP needs no skill.
    let mut client = McpClient::spawn_raw();
    client.send(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"0.1"}}}"#,
    );
    let resp = client.recv_until_response(1);
    let instructions = resp["result"]["instructions"]
        .as_str()
        .expect("initialize result must carry instructions");
    assert!(
        instructions.contains("fetch_messages") && instructions.contains("swarm_manual"),
        "instructions must teach the poll loop and point to swarm_manual"
    );
}

#[test]
fn create_swarm_twice_errors_cleanly() {
    let mut client = McpClient::spawn();
    let first = client.tool_call(20, "create_swarm", serde_json::json!({ "name": "twice1" }));
    let first_json = tool_result_json(&first).expect("first create_swarm should succeed");
    assert!(first_json["swarm"].as_str().unwrap().starts_with("ahsw"));
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
            .starts_with("ahsw"),
        "expected a swarm id, got: {result}"
    );
}

#[test]
fn create_swarm_without_name_mints_random() {
    // `name` is optional, mirroring the CLI and the plugin/pi front-ends:
    // omit it and the server mints a random `word-word` name (and nickname).
    let mut client = McpClient::spawn();
    let resp = client.tool_call(100, "create_swarm", serde_json::json!({}));
    let result = tool_result_json(&resp).expect("nameless create should succeed");
    assert!(
        result["swarm"]
            .as_str()
            .unwrap_or_default()
            .starts_with("ahsw"),
        "expected a swarm id, got: {result}"
    );
    assert!(
        !result["name"].as_str().unwrap_or_default().is_empty(),
        "expected a minted name, got: {result}"
    );
    assert!(
        !result["nickname"].as_str().unwrap_or_default().is_empty(),
        "expected a minted nickname, got: {result}"
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

    // FROM_START dodges the implicit cursor to inspect the whole buffer.
    let fetched = tool_result_json(&client.tool_call(
        102,
        "fetch_messages",
        serde_json::json!({ "after": FROM_START }),
    ))
    .expect("fetch");
    let msgs = fetched["messages"].as_array().unwrap();
    assert!(
        msgs.iter().any(|msg| msg["id"].as_str() == Some(&id)),
        "buffer should contain the just-sent message, got {msgs:?}"
    );
}

#[test]
fn fetch_messages_with_out_of_range_after_is_graceful() {
    // A cursor past the newest seq (nothing newer) returns an empty batch
    // without crashing; `after: 0` returns the whole buffer. Both must be
    // well-formed arrays.
    let mut client = McpClient::spawn();
    client.create_and_get_swarm(110);
    let _ = client.tool_call(111, "send_message", serde_json::json!({ "text": "a" }));

    let far_future = tool_result_json(&client.tool_call(
        112,
        "fetch_messages",
        serde_json::json!({ "after": 1_000_000_u64 }),
    ))
    .expect("fetch with far-future cursor");
    assert!(
        far_future["messages"].as_array().is_some_and(Vec::is_empty),
        "a cursor past the newest seq returns an empty batch"
    );

    let from_start = tool_result_json(&client.tool_call(
        113,
        "fetch_messages",
        serde_json::json!({ "after": FROM_START }),
    ))
    .expect("fetch from start");
    assert!(from_start["messages"].is_array());
}

#[test]
fn fetch_messages_wait_ms_long_polls_then_times_out_empty() {
    // The `wait_ms` arg threads MCP → session → embed → daemon and back over
    // stdio: against a lone session with no new traffic, a short long-poll
    // blocks ~the wait and returns a well-formed empty batch (not an error,
    // not an immediate return). The blocking-resolves-on-traffic behavior is
    // covered behaviorally at the embed layer (session.rs); here we assert the
    // wire round-trip + timeout shape. (Cursor first advanced past history.)
    let mut client = McpClient::spawn();
    client.create_and_get_swarm(130);
    // Advance the implicit cursor past any startup/self events.
    let _ = client.tool_call(131, "fetch_messages", serde_json::json!({}));

    let started = Instant::now();
    let resp = tool_result_json(&client.tool_call(
        132,
        "fetch_messages",
        serde_json::json!({ "wait_ms": 500 }),
    ))
    .expect("long-poll fetch returns a JSON result");
    let elapsed = started.elapsed();

    assert!(
        resp["messages"].as_array().is_some_and(Vec::is_empty),
        "no traffic → empty messages: {resp}"
    );
    assert!(
        elapsed >= Duration::from_millis(300),
        "wait_ms must actually block (~500ms), took {elapsed:?}"
    );
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
    // The server auto-tracks an implicit `seq` cursor: `fetch_messages`
    // advances it past the last returned event. A self-send surfaces once
    // (with self:true, matching the live stream), so the *first* cursor-less
    // fetch sees the self-sends; the *next* cursor-less fetch is empty (the
    // cursor advanced past them). Explicit `after` overrides the cursor.
    let mut client = McpClient::spawn();
    client.create_and_get_swarm(200);

    // Capture the seq just before the sends so we can replay from there.
    let baseline = tool_result_json(&client.tool_call(
        205,
        "fetch_messages",
        serde_json::json!({ "after": FROM_START }),
    ))
    .expect("baseline fetch");
    let before_seq = baseline["current_seq"].as_u64().unwrap_or(0);

    for (i, body) in ["one", "two", "three"].iter().enumerate() {
        client.tool_call(
            201 + i as u64,
            "send_message",
            serde_json::json!({ "text": body }),
        );
    }

    // Explicit replay from before the sends yields the three self-sends
    // (now surfaced, each self:true), overriding the implicit cursor.
    let replay = tool_result_json(&client.tool_call(
        211,
        "fetch_messages",
        serde_json::json!({ "after": before_seq }),
    ))
    .expect("explicit replay");
    let bodies: Vec<&str> = replay["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|event| event["body"].as_str())
        .collect();
    assert!(
        ["one", "two", "three"]
            .iter()
            .all(|body| bodies.contains(body)),
        "explicit after must replay the three self-sends, got {bodies:?}"
    );
    let cursor = replay["current_seq"]
        .as_u64()
        .expect("current_seq should be set when batch non-empty");
    assert_eq!(
        replay["messages"].as_array().unwrap().last().unwrap()["seq"].as_u64(),
        Some(cursor),
        "current_seq must match the last event's seq in the batch"
    );

    // Send one more, then fetch with the cursor → only the new message.
    let _ = client.tool_call(220, "send_message", serde_json::json!({ "text": "four" }));
    let cursor2 = {
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        loop {
            let second = tool_result_json(&client.tool_call(
                221,
                "fetch_messages",
                serde_json::json!({ "after": cursor }),
            ))
            .expect("delta fetch");
            let surfaced_four = second["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event["body"].as_str() == Some("four"));
            if surfaced_four {
                break second["current_seq"].as_u64().unwrap();
            }
            assert!(
                Instant::now() < deadline,
                "delta fetch never surfaced 'four'"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    assert!(
        cursor2 > cursor,
        "current_seq must advance past the old cursor"
    );

    // Fetch again with the new cursor — nothing newer, empty batch,
    // null current_seq.
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
        third["current_seq"].is_null(),
        "empty batch → null current_seq, got: {}",
        third["current_seq"]
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
    // fully caught up). FROM_START keeps the implicit cursor out of
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
            serde_json::json!({ "after": FROM_START }),
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

// ─── task + roster ───────────────────────────────────────────────

const MCP_EXCHANGE_ID: &str = "550e8400-e29b-41d4-a716-446655440000";

/// A task `offer` sent (via `send_exchange`) by the joiner to the creator
/// surfaces on the creator's `fetch_messages` as the stream-shaped
/// `event:"exchange"` record with the `to`/`exchange_id`/`kind`/`phase`/`body`
/// fields (plus `display` and `self`).
#[test]
fn send_exchange_surfaces_to_addressee_via_fetch() {
    let (mut creator, mut joiner, _swarm, creator_nick) = create_pair(700);

    let sent = tool_result_json(&joiner.tool_call(
        710,
        "send_exchange",
        serde_json::json!({
            "to": creator_nick,
            "exchange_id": MCP_EXCHANGE_ID,
            "kind": "handover",
            "phase": "offer",
            "text": "## Task\nport it",
        }),
    ))
    .expect("send_exchange should succeed");
    // The echo is the authoritative task record.
    assert_eq!(sent["message"]["type"], "exchange");
    assert_eq!(sent["message"]["phase"], "offer");
    assert_eq!(sent["message"]["to"], creator_nick);

    // The creator fetches and sees the task leg addressed to it.
    let deadline = Instant::now() + MSG_TIMEOUT;
    let mut probe = 720;
    let task = loop {
        let fetched = tool_result_json(&creator.tool_call(
            probe,
            "fetch_messages",
            serde_json::json!({ "after": FROM_START }),
        ))
        .expect("fetch_messages must succeed");
        let found = fetched["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|event| event["event"] == "exchange")
            .cloned();
        if let Some(found) = found {
            break found;
        }
        assert!(
            Instant::now() < deadline,
            "creator never received the task; buffer: {}",
            fetched["messages"]
        );
        probe += 1;
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(task["exchange_id"], MCP_EXCHANGE_ID);
    assert_eq!(task["kind"], "handover");
    assert_eq!(task["phase"], "offer");
    assert_eq!(task["to"], creator_nick);
    assert_eq!(task["body"], "## Task\nport it");
}

/// `send_exchange --phase offer` to a nickname that is not a current
/// participant is rejected with an `unknown participant` error.
#[test]
fn send_exchange_offer_to_unknown_participant_errors() {
    let mut client = McpClient::spawn();
    let _ = client.create_and_get_swarm(730);
    let resp = client.tool_call(
        731,
        "send_exchange",
        serde_json::json!({
            "to": "ghost-peer",
            "exchange_id": MCP_EXCHANGE_ID,
            "kind": "handover",
            "phase": "offer",
            "text": "brief",
        }),
    );
    let err = tool_error(&resp).expect("task offer to unknown participant should error");
    assert!(
        err.contains("unknown participant"),
        "expected unknown-participant error, got: {err}"
    );
}

/// `swarm_info` now reports the participant count and the live roster
/// (each peer's nickname, recency, quiet flag, reach tag).
#[test]
fn swarm_info_reports_participant_roster() {
    let (mut creator, mut joiner, _swarm, _creator_nick) = create_pair(740);
    let joiner_nick = tool_result_json(&joiner.tool_call(741, "swarm_info", serde_json::json!({})))
        .expect("joiner swarm_info")["nickname"]
        .as_str()
        .expect("nickname")
        .to_string();

    // Poll the creator's roster until it has converged to both members.
    let deadline = Instant::now() + MSG_TIMEOUT;
    let mut probe = 742;
    let info = loop {
        let info = tool_result_json(&creator.tool_call(probe, "swarm_info", serde_json::json!({})))
            .expect("creator swarm_info");
        if info["participant_count"].as_u64() == Some(2) {
            break info;
        }
        assert!(
            Instant::now() < deadline,
            "creator roster never converged: {info}"
        );
        probe += 1;
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(info["participant_count"], 2);
    let participants = info["participants"].as_array().expect("participants array");
    assert!(
        participants
            .iter()
            .any(|entry| entry["nickname"].as_str() == Some(joiner_nick.as_str())),
        "creator roster should list the joiner: {participants:?}"
    );
    for entry in participants {
        assert!(entry["nickname"].is_string());
        assert!(entry.get("last_seen_secs_ago").is_some());
        assert!(entry["quiet"].is_boolean());
        let reach = entry["reach"].as_str().expect("reach is a string");
        assert!(
            reach == "direct" || reach == "gossip",
            "unexpected reach: {reach}"
        );
    }
}

/// Self-reported model/harness round-trips both ways: a creator and joiner
/// each pass their own `model`/`harness`, and each sees the *other's* values
/// in its `swarm_info` roster. Proves both `create_swarm` and `join_swarm`
/// plumb the fields through to the announced presence.
#[test]
fn create_and_join_self_report_model_harness() {
    let mut creator = McpClient::spawn();
    let mut joiner = McpClient::spawn();

    let created = tool_result_json(&creator.tool_call(
        760,
        "create_swarm",
        serde_json::json!({ "name": "mcpmeta", "model": "Opus 4.8", "harness": "Claude Code" }),
    ))
    .expect("create_swarm must succeed");
    let swarm = created["swarm"].as_str().expect("swarm id").to_string();
    let creator_nick = created["nickname"].as_str().expect("nickname").to_string();

    let join_result = tool_result_json(&joiner.tool_call(
        761,
        "join_swarm",
        serde_json::json!({ "swarm": swarm, "model": "Sonnet 4.6", "harness": "pi" }),
    ))
    .expect("join_swarm must succeed");
    let joiner_nick = join_result["nickname"]
        .as_str()
        .expect("nickname")
        .to_string();

    // Each side polls its roster for the *other* peer, then asserts the
    // self-reported metadata surfaced.
    let find_peer = |client: &mut McpClient, base: u64, peer: &str| -> serde_json::Value {
        let deadline = Instant::now() + MSG_TIMEOUT;
        let mut probe = base;
        loop {
            let info =
                tool_result_json(&client.tool_call(probe, "swarm_info", serde_json::json!({})))
                    .expect("swarm_info");
            if let Some(entry) = info["participants"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|entry| entry["nickname"].as_str() == Some(peer))
                .cloned()
            {
                break entry;
            }
            assert!(Instant::now() < deadline, "{peer} never surfaced: {info}");
            probe += 1;
            std::thread::sleep(Duration::from_millis(100));
        }
    };

    let joiner_entry = find_peer(&mut creator, 762, &joiner_nick);
    assert_eq!(
        joiner_entry["model"].as_str(),
        Some("Sonnet 4.6"),
        "{joiner_entry}"
    );
    assert_eq!(
        joiner_entry["harness"].as_str(),
        Some("pi"),
        "{joiner_entry}"
    );

    let creator_entry = find_peer(&mut joiner, 780, &creator_nick);
    assert_eq!(
        creator_entry["model"].as_str(),
        Some("Opus 4.8"),
        "{creator_entry}"
    );
    assert_eq!(
        creator_entry["harness"].as_str(),
        Some("Claude Code"),
        "{creator_entry}"
    );
}

/// Loopback timings so the advertise→discover round runs in seconds: short
/// co-host grace (the advertiser becomes the directory beacon fast) and
/// frequent re-ads (so the discoverer's collection window catches one).
const DIR_FLAGS: [(&str, &str); 4] = [
    ("--directory-private", ""),
    ("--beacon-cohost-grace-secs", "2"),
    ("--advertise-interval-secs", "2"),
    ("--alive-timeout-secs", "5"),
];

/// `discover_swarms` finds a swarm advertised into the same directory. A CLI
/// advertiser lists itself over the loopback ladder; an MCP server (also on
/// loopback via the hidden `--directory-private`) browses and sees it.
#[test]
fn discover_swarms_finds_an_advertised_swarm() {
    let adv_log = tmp_log("mcp-disc-adv");
    let adv_file = File::create(&adv_log).unwrap();
    let mut advertiser = test_cmd()
        .args([
            "create",
            "--advertise",
            "mcdir",
            "--name",
            "mcpdisc",
            "--nickname",
            "adv",
            "--no-interactive",
            "--output",
            "json",
        ])
        .args(flag_args(&DIR_FLAGS))
        .stdout(Stdio::from(adv_file.try_clone().unwrap()))
        .stderr(Stdio::from(adv_file))
        .spawn()
        .expect("spawn advertiser");

    // Wait until the advertiser has started (its `ready` JSON carries `swarm`)
    // so the discoverer's first window isn't spent waiting for it to come up.
    let up_deadline = Instant::now() + CONNECT_TIMEOUT;
    while Instant::now() < up_deadline
        && !fs::read_to_string(&adv_log)
            .unwrap_or_default()
            .contains("\"swarm\"")
    {
        std::thread::sleep(POLL);
    }

    let mut client = McpClient::spawn_with_args(&["--directory-private"]);
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut id = 800;
    let found = loop {
        let resp = client.tool_call(
            id,
            "discover_swarms",
            serde_json::json!({ "directory": "mcdir" }),
        );
        let json = tool_result_json(&resp).expect("discover_swarms returns a result");
        let hit = json["swarms"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|listing| listing["name"].as_str() == Some("mcpdisc"));
        if hit {
            break true;
        }
        if Instant::now() > deadline {
            break false;
        }
        id += 1;
    };

    let _ = advertiser.kill();
    let _ = advertiser.wait();
    let _ = fs::remove_file(&adv_log);
    assert!(found, "discover_swarms never found the advertised swarm");
}

/// `ping` reports a round-trip time for a linked peer. The pinger uses a short
/// window (hidden `--ping-window-secs`) so the round finalizes in seconds.
#[test]
fn ping_reports_rtt_to_a_peer() {
    let (mut creator, mut joiner, _swarm, _creator_nick) =
        create_pair_with(900, &["--ping-window-secs", "2"]);
    let joiner_nick = tool_result_json(&joiner.tool_call(901, "swarm_info", serde_json::json!({})))
        .expect("joiner swarm_info")["nickname"]
        .as_str()
        .expect("nickname")
        .to_string();

    // One round may miss if the mesh just linked; retry within budget.
    let deadline = Instant::now() + MSG_TIMEOUT;
    let mut id = 902;
    let rtt = loop {
        let resp = creator.tool_call(id, "ping", serde_json::json!({}));
        let json = tool_result_json(&resp).expect("ping returns a result");
        let hit = json["peers"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|peer| peer["nickname"].as_str() == Some(joiner_nick.as_str()))
            .and_then(|peer| peer["rtt_ms"].as_u64());
        if let Some(rtt) = hit {
            break Some(rtt);
        }
        if Instant::now() > deadline {
            break None;
        }
        id += 1;
    };
    assert!(rtt.is_some(), "ping never reported an RTT for the joiner");
}
