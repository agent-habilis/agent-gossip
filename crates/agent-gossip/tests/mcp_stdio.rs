//! Integration tests: `agent-gossip mcp` over stdio.
//!
//! Spawns the binary, pipes in JSON-RPC, asserts the server's
//! responses. These are the reliability guarantees we make at the
//! MCP surface.

use agent_gossip_test_fixtures as common;

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
            .expect("spawn agent-gossip mcp");
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
            .expect("spawn agent-gossip mcp");
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
        reason = "ergonomic test helper; the 50-odd call sites pass json! literals by value, and taking &Value would spray a reference sigil across every one for no gain in test code"
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

    /// Shorthand for the `create_gossip` + extract-id ritual that
    /// opens almost every integration test. Returns
    /// `(mesh_id, nickname)`.
    fn create_and_get_mesh(&mut self, id: u64) -> (String, String) {
        let created = tool_result_json(&self.tool_call(
            id,
            "create_gossip",
            serde_json::json!({ "name": "mcptest" }),
        ))
        .expect("create_gossip must succeed");
        let mesh = created["gossip"]
            .as_str()
            .expect("create_gossip result must include mesh")
            .to_string();
        assert_eq!(
            created["name"].as_str(),
            Some("mcptest"),
            "create_gossip result must echo back the name"
        );
        let nickname = created["nickname"]
            .as_str()
            .expect("create_gossip result must include nickname")
            .to_string();
        (mesh, nickname)
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
    // Either a JSON-RPC error (invalid args / not in mesh) or
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

/// Spawn two MCP clients, have the first create a private mesh
/// and the second join it, then block until gossip has linked them
/// (creator sees the joiner's `joined` presence in its buffer).
/// Returns `(creator, joiner, mesh_id, creator_nickname)`.
///
/// Id reservations (so tests can't collide with our probes):
/// - `base_id + 0` — creator's `create_gossip`
/// - `base_id + 1` — joiner's `join_gossip`
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
    let (mesh, creator_nick) = creator.create_and_get_mesh(base_id);
    tool_result_json(&joiner.tool_call(
        base_id + 1,
        "join_gossip",
        serde_json::json!({ "gossip": mesh.clone() }),
    ))
    .expect("join_gossip must succeed");

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

    // Linkage above only proves the broadcast path carries the creator's
    // traffic. A *directed* call (`a2a_call --to`) is additionally sealed to the
    // creator's published X25519 key, which the daemon refuses to send without
    // ("cards still propagating") and which replicates through the `meta`
    // document on its own schedule. So wait for the card too, or every directed
    // call in this binary races it — the subprocess twin of
    // `InProcNode::await_peer_card`.
    let card = format!("/peers/{creator_nick}/card");
    assert!(
        poll_doc(&mut joiner, probe_id, "get_meta", |doc| doc
            .pointer(&card)
            .is_some()),
        "creator's card never reached the joiner's meta doc"
    );

    (creator, joiner, mesh, creator_nick)
}

// ─── password ────────────────────────────────────────────────────

/// A passworded mesh over MCP: `create_gossip` takes `password`;
/// `join_gossip` without it (or with the wrong one) is an `invalid_params`
/// error naming the requirement — the server never prompts — and the right
/// password joins.
#[test]
fn join_mesh_requires_the_password() {
    let mut creator = McpClient::spawn();
    let created = tool_result_json(&creator.tool_call(
        7000,
        "create_gossip",
        serde_json::json!({ "name": "mcp-pw", "password": "hunter2" }),
    ))
    .expect("passworded create_gossip must succeed");
    let mesh = created["gossip"].as_str().expect("mesh id").to_string();

    let mut joiner = McpClient::spawn();
    let missing = tool_error(&joiner.tool_call(
        7001,
        "join_gossip",
        serde_json::json!({ "gossip": mesh.clone() }),
    ))
    .expect("join without password must error");
    assert!(
        missing.contains("password-protected"),
        "error must name the requirement: {missing}"
    );

    let wrong = tool_error(&joiner.tool_call(
        7002,
        "join_gossip",
        serde_json::json!({ "gossip": mesh.clone(), "password": "hunter3" }),
    ))
    .expect("join with a wrong password must error");
    assert!(
        wrong.contains("wrong password"),
        "error must say wrong password: {wrong}"
    );

    tool_result_json(&joiner.tool_call(
        7003,
        "join_gossip",
        serde_json::json!({ "gossip": mesh, "password": "hunter2" }),
    ))
    .expect("join with the right password must succeed");
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
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"create_gossip","arguments":{"name":"mcptest"}}}"#,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"send_broadcast","arguments":{"text":"sanity"}}}"#,
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"leave_gossip","arguments":{}}}"#,
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
        (10, "send_broadcast"),
        (11, "fetch_messages"),
        (12, "gossip_info"),
    ] {
        let args = if tool == "send_broadcast" {
            serde_json::json!({ "text": "hi" })
        } else {
            serde_json::json!({})
        };
        let resp = client.tool_call(id, tool, args);
        let err = tool_error(&resp)
            .unwrap_or_else(|| panic!("{tool} without session should error, got: {resp}"));
        assert!(
            err.contains("not in a gossip"),
            "{tool}: expected 'not in a gossip' error, got: {err}"
        );
    }
}

#[test]
fn mesh_version_works_without_a_session() {
    let mut client = McpClient::spawn();
    // Unlike the messaging tools, version is a local check — no mesh needed.
    let resp = client.tool_call(30, "gossip_version", serde_json::json!({}));
    let json = tool_result_json(&resp).expect("gossip_version should return a JSON result");
    assert!(
        json["version"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "gossip_version must report a non-empty build string, got: {json}"
    );
    // MCP carries no skill of its own, so the version result is version-only —
    // the former skill-drift fields are gone.
    assert!(
        json.get("skill_up_to_date").is_none() && json.get("skill_state").is_none(),
        "gossip_version must not report skill fields over MCP, got: {json}"
    );
}

#[test]
fn mesh_manual_returns_the_manual_without_a_session() {
    let mut client = McpClient::spawn();
    let resp = client.tool_call(31, "gossip_manual", serde_json::json!({}));
    let text = tool_result_text(&resp).expect("gossip_manual should return text content");
    assert!(
        text.contains("COMMANDS") && text.contains("JSON EVENTS"),
        "gossip_manual must return the full manual, got {} chars",
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
        instructions.contains("fetch_messages") && instructions.contains("gossip_manual"),
        "instructions must teach the poll loop and point to gossip_manual"
    );
}

#[test]
fn create_mesh_twice_errors_cleanly() {
    let mut client = McpClient::spawn();
    let first = client.tool_call(20, "create_gossip", serde_json::json!({ "name": "twice1" }));
    let first_json = tool_result_json(&first).expect("first create_gossip should succeed");
    assert!(first_json["gossip"].as_str().unwrap().starts_with("💬"));
    assert_eq!(first_json["name"].as_str(), Some("twice1"));

    let second = client.tool_call(21, "create_gossip", serde_json::json!({ "name": "twice2" }));
    let err = tool_error(&second).expect("second create_gossip should error, but got success");
    assert!(
        err.contains("already in gossip"),
        "expected 'already in gossip' error, got: {err}"
    );
}

#[test]
fn join_mesh_idempotent_for_same_mesh() {
    let mut client = McpClient::spawn();
    let (mesh, nickname) = client.create_and_get_mesh(30);

    // Idempotent: join the mesh we just created with the same
    // nickname → no error, same handle back.
    let rejoin = client.tool_call(
        31,
        "join_gossip",
        serde_json::json!({ "gossip": mesh.clone(), "nickname": nickname.clone() }),
    );
    let rejoin_json = tool_result_json(&rejoin).unwrap_or_else(|| {
        panic!("idempotent join_gossip should succeed, but got error response: {rejoin}")
    });
    assert_eq!(rejoin_json["gossip"].as_str(), Some(mesh.as_str()));
    assert_eq!(rejoin_json["nickname"].as_str(), Some(nickname.as_str()));

    // Different nickname on the same mesh → error.
    let conflict = client.tool_call(
        32,
        "join_gossip",
        serde_json::json!({ "gossip": mesh, "nickname": "someone-else" }),
    );
    let err = tool_error(&conflict).expect("different-nick join should error");
    assert!(
        err.contains("already in gossip"),
        "expected 'already in gossip' error, got: {err}"
    );
}

#[test]
fn join_with_invalid_input_errors_gracefully() {
    let mut client = McpClient::spawn();
    let resp = client.tool_call(
        40,
        "join_gossip",
        serde_json::json!({ "gossip": "not-a-mesh-id" }),
    );
    let err = tool_error(&resp).expect("invalid join should error");
    // Exact wording can vary; just confirm we got an error and no
    // crash. Server must still accept the next request.
    assert!(!err.is_empty(), "error message must not be empty");

    // Prove the server is still alive: call a benign tool.
    let info = client.tool_call(41, "gossip_info", serde_json::json!({}));
    let err2 = tool_error(&info).expect("gossip_info with no session should error");
    assert!(err2.contains("not in a gossip"));
}

#[test]
fn leave_without_session_is_noop() {
    let mut client = McpClient::spawn();
    let resp = client.tool_call(50, "leave_gossip", serde_json::json!({}));
    // Docs say: "no-op if not in one." Must succeed.
    let json = tool_result_json(&resp).expect("leave with no session should succeed");
    assert_eq!(json["ok"], serde_json::json!(true));
}

#[test]
fn leave_then_create_cycle_works_within_one_server() {
    let mut client = McpClient::spawn();
    let first = tool_result_json(&client.tool_call(
        60,
        "create_gossip",
        serde_json::json!({ "name": "cycle1" }),
    ))
    .expect("create");
    let first_mesh = first["gossip"].as_str().unwrap().to_string();

    let _ = client.tool_call(61, "leave_gossip", serde_json::json!({}));

    let second = tool_result_json(&client.tool_call(
        62,
        "create_gossip",
        serde_json::json!({ "name": "cycle2" }),
    ))
    .expect("create after leave");
    assert_ne!(
        second["gossip"].as_str().unwrap(),
        first_mesh,
        "second create should mint a fresh mesh id"
    );
}

#[test]
fn send_broadcast_without_session_errors() {
    let mut client = McpClient::spawn();
    let resp = client.tool_call(
        70,
        "send_broadcast",
        serde_json::json!({ "text": "orphan" }),
    );
    let err = tool_error(&resp).expect("send_broadcast without session should error");
    assert!(err.contains("not in a gossip"));
}

#[test]
fn create_mesh_with_granular_relay_succeeds() {
    // Granular lookups: naming `relay` opts into it directly, the same way
    // the CLI `--relay` flag does — the old "relay requires public" rule is
    // gone, so a relay-only (network:private) mesh now creates fine.
    let mut client = McpClient::spawn();
    let resp = client.tool_call(
        80,
        "create_gossip",
        serde_json::json!({ "name": "relayp", "network": "private", "relay": "https://relay.example/" }),
    );
    let result = tool_result_json(&resp).expect("granular relay create should succeed");
    assert!(
        result["gossip"]
            .as_str()
            .unwrap_or_default()
            .starts_with("💬"),
        "expected a mesh id, got: {result}"
    );
}

#[test]
fn create_mesh_without_name_mints_random() {
    // `name` is optional, mirroring the CLI and the plugin/pi front-ends:
    // omit it and the server mints a random `word-word` name (and nickname).
    let mut client = McpClient::spawn();
    let resp = client.tool_call(100, "create_gossip", serde_json::json!({}));
    let result = tool_result_json(&resp).expect("nameless create should succeed");
    assert!(
        result["gossip"]
            .as_str()
            .unwrap_or_default()
            .starts_with("💬"),
        "expected a mesh id, got: {result}"
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
fn create_mesh_with_unknown_network_errors() {
    let mut client = McpClient::spawn();
    let resp = client.tool_call(
        90,
        "create_gossip",
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
    let (_mesh, _) = client.create_and_get_mesh(100);

    let sent = client.tool_call(101, "send_broadcast", serde_json::json!({ "text": "" }));
    let sent_json = tool_result_json(&sent).expect("empty-body send should succeed");
    let id = sent_json["id"].as_str().unwrap().to_string();
    assert!(!id.is_empty());
    // send_broadcast now returns the full echo inline — the agent
    // should never need a follow-up fetch for its own send.
    let echo = sent_json
        .get("message")
        .expect("send_broadcast must return an echo");
    assert_eq!(echo["id"].as_str(), Some(id.as_str()));
    // The frame body carries the serialized A2A payload; the sent text
    // (empty here) is its first text part, and the payload's messageId is
    // the frame id.
    let payload: serde_json::Value =
        serde_json::from_str(echo["body"].as_str().expect("frame body is a string"))
            .expect("frame body is a serialized a2a message");
    assert_eq!(payload["parts"][0]["text"].as_str(), Some(""));
    assert_eq!(payload["messageId"].as_str(), Some(id.as_str()));
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
    client.create_and_get_mesh(110);
    let _ = client.tool_call(111, "send_broadcast", serde_json::json!({ "text": "a" }));

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
fn fetch_messages_long_parks_then_times_out_empty() {
    // The `long` arg threads MCP → session → api → daemon and back over
    // stdio: against a lone session with no new traffic, a long-poll parks
    // for ~the daemon's park cap (shrunk to 500ms via the hidden flag) and
    // returns a well-formed empty batch (not an error, not an immediate
    // return). The parking-resolves-on-traffic behavior is covered
    // behaviorally at the api layer (session.rs); here we assert the wire
    // round-trip + timeout shape. (Cursor first advanced past history.)
    let mut client = McpClient::spawn_with_args(&["--longpoll-max-ms", "500"]);
    client.create_and_get_mesh(130);

    // A lone session still emits one async join-time event: the daemon publishes
    // its own agent-card to the meta channel. A single immediate fetch to
    // "advance the cursor" races that publish — if the card surfaces after it,
    // the long-poll below catches it and the empty assertion flakes. Long-poll
    // repeatedly instead: each call either consumes pending startup traffic or
    // parks the full window empty. The first poll that parks empty proves
    // startup drained AND is the park-then-timeout behavior under test. (`long`
    // resolves early on traffic, so a mid-park self-card is returned, not
    // missed.)
    for request_id in 131..139 {
        let started = Instant::now();
        let resp = tool_result_json(&client.tool_call(
            request_id,
            "fetch_messages",
            serde_json::json!({ "long": true }),
        ))
        .expect("long-poll fetch returns a JSON result");
        if resp["messages"].as_array().is_some_and(Vec::is_empty) {
            assert!(
                started.elapsed() >= Duration::from_millis(300),
                "long must actually park (~500ms cap), took {:?}",
                started.elapsed()
            );
            return;
        }
    }
    panic!("startup traffic never drained after 8 long-polls");
}

#[test]
fn send_broadcast_reaches_the_mesh() {
    // `send_broadcast` reaches every member; `send_msg` reaches one. Confirm
    // the broadcast path succeeds.
    let mut client = McpClient::spawn();
    client.create_and_get_mesh(120);
    let resp = client.tool_call(
        121,
        "send_broadcast",
        serde_json::json!({ "text": "hello mesh" }),
    );
    let json = tool_result_json(&resp).expect("broadcast send should succeed");
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
    client.create_and_get_mesh(200);

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
            "send_broadcast",
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
    let _ = client.tool_call(220, "send_broadcast", serde_json::json!({ "text": "four" }));
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

// ─── task + roster ───────────────────────────────────────────────

/// Creating a task (via the `a2a_call` tool, `message/send`) from the joiner
/// to the creator: the worker mints the id and returns a submitted `Task`, and
/// the creator (worker) surfaces the incoming message on `fetch_messages` as a
/// `message`-kind `event:"task"` record.
#[test]
fn task_creation_surfaces_to_worker_via_fetch() {
    let (mut creator, mut joiner, mesh, creator_nick) = create_pair(700);

    let raw = joiner.tool_call(
        710,
        "a2a_call",
        serde_json::json!({
            "to": creator_nick,
            "method": "SendMessage",
            "params": { "message": {
                "messageId": "550e8400-e29b-41d4-a716-446655440000",
                "role": "ROLE_USER",
                "parts": [{ "text": "## Task\nport it" }],
                "contextId": mesh,
            }},
        }),
    );
    let resp = tool_result_json(&raw)
        .unwrap_or_else(|| panic!("a2a_call should succeed; raw response: {raw}"));
    // The worker mints the task id and returns a submitted Task (v1.0
    // SendMessageResponse oneof: `{"task": <Task>}`).
    assert!(resp["result"]["task"].is_object(), "got: {resp}");
    let task_id = resp["result"]["task"]["id"]
        .as_str()
        .expect("server-minted task id")
        .to_string();

    // The creator (worker) fetches and sees the incoming message as a task leg.
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
            .find(|event| event["event"] == "task")
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
    assert_eq!(task["task_id"], task_id.as_str());
    assert_eq!(task["kind"], "message");
    assert_eq!(task["to"], creator_nick);
    assert_eq!(task["body"], "## Task\nport it");
}

/// A directed `message/send` (task creation) to a nickname that is not a
/// current peer is rejected with an `unknown peer` error.
#[test]
fn task_creation_to_unknown_peer_errors() {
    let mut client = McpClient::spawn();
    let (mesh, _nick) = client.create_and_get_mesh(730);
    let resp = client.tool_call(
        731,
        "a2a_call",
        serde_json::json!({
            "to": "ghost-peer",
            "method": "SendMessage",
            "params": { "message": {
                "messageId": "550e8400-e29b-41d4-a716-446655440000",
                "role": "ROLE_USER",
                "parts": [{ "text": "brief" }],
                "contextId": mesh,
            }},
        }),
    );
    let err = tool_error(&resp).expect("task creation to unknown peer should error");
    assert!(
        err.contains("unknown peer"),
        "expected unknown-peer error, got: {err}"
    );
}

/// `gossip_info` now reports the peer count and the live roster
/// (each peer's nickname, recency, quiet flag, reach tag).
#[test]
fn mesh_info_reports_peer_roster() {
    let (mut creator, mut joiner, _mesh, _creator_nick) = create_pair(740);
    let joiner_nick =
        tool_result_json(&joiner.tool_call(741, "gossip_info", serde_json::json!({})))
            .expect("joiner gossip_info")["nickname"]
            .as_str()
            .expect("nickname")
            .to_string();

    // Poll the creator's roster until it has converged to both members.
    let deadline = Instant::now() + MSG_TIMEOUT;
    let mut probe = 742;
    let info = loop {
        let info =
            tool_result_json(&creator.tool_call(probe, "gossip_info", serde_json::json!({})))
                .expect("creator gossip_info");
        if info["peer_count"].as_u64() == Some(2) {
            break info;
        }
        assert!(
            Instant::now() < deadline,
            "creator roster never converged: {info}"
        );
        probe += 1;
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(info["peer_count"], 2);
    let peers = info["peers"].as_array().expect("peers array");
    assert!(
        peers
            .iter()
            .any(|entry| entry["nickname"].as_str() == Some(joiner_nick.as_str())),
        "creator roster should list the joiner: {peers:?}"
    );
    for entry in peers {
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

/// Loopback timings so the advertise→discover round runs in seconds: short
/// co-host grace (the advertiser becomes the directory beacon fast) and
/// frequent re-ads (so the discoverer's collection window catches one).
const DIR_FLAGS: [(&str, &str); 4] = [
    ("--directory-private", ""),
    ("--beacon-cohost-grace-secs", "2"),
    ("--advertise-interval-secs", "2"),
    ("--alive-timeout-secs", "5"),
];

/// `discover_gossips` finds a mesh advertised into the same directory. A CLI
/// advertiser lists itself over the loopback ladder; an MCP server (also on
/// loopback via the hidden `--directory-private`) browses and sees it.
#[test]
fn discover_meshes_finds_an_advertised_mesh() {
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
        ])
        .args(flag_args(&DIR_FLAGS))
        .stdout(Stdio::from(adv_file.try_clone().unwrap()))
        .stderr(Stdio::from(adv_file))
        .spawn()
        .expect("spawn advertiser");

    // Wait until the advertiser has started (its `ready` JSON carries `mesh`)
    // so the discoverer's first window isn't spent waiting for it to come up.
    let up_deadline = Instant::now() + CONNECT_TIMEOUT;
    while Instant::now() < up_deadline
        && !fs::read_to_string(&adv_log)
            .unwrap_or_default()
            .contains("\"gossip\"")
    {
        std::thread::sleep(POLL);
    }

    let mut client = McpClient::spawn_with_args(&["--directory-private"]);
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut id = 800;
    let found = loop {
        let resp = client.tool_call(
            id,
            "discover_gossips",
            serde_json::json!({ "directory": "mcdir" }),
        );
        let json = tool_result_json(&resp).expect("discover_gossips returns a result");
        let hit = json["gossips"]
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
    assert!(found, "discover_gossips never found the advertised gossip");
}

/// `ping` reports a round-trip time for a linked peer. The pinger uses a short
/// window (hidden `--ping-window-secs`) so the round finalizes in seconds.
#[test]
fn ping_reports_rtt_to_a_peer() {
    let (mut creator, mut joiner, _mesh, _creator_nick) =
        create_pair_with(900, &["--ping-window-secs", "2"]);
    let joiner_nick =
        tool_result_json(&joiner.tool_call(901, "gossip_info", serde_json::json!({})))
            .expect("joiner gossip_info")["nickname"]
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

/// Poll a read tool (`get_state` / `get_meta`) on `client` until its `document`
/// satisfies `pred` or `MSG_TIMEOUT` elapses.
fn poll_doc(
    client: &mut McpClient,
    base_id: u64,
    tool: &str,
    mut pred: impl FnMut(&serde_json::Value) -> bool,
) -> bool {
    let deadline = Instant::now() + MSG_TIMEOUT;
    let mut id = base_id;
    loop {
        let resp = tool_result_json(&client.tool_call(id, tool, serde_json::json!({})))
            .unwrap_or_else(|| panic!("{tool} must succeed"));
        if pred(&resp["document"]) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        id += 1;
        std::thread::sleep(POLL);
    }
}

/// Both shared-state channels round-trip over MCP: an `apply_*_patch` on one
/// client is reflected by the other client's `get_*`, and the two channels stay
/// isolated (a `state` key never appears in `get_meta`, nor vice versa). Proves
/// the meta MCP tools reach a real, independent second channel.
#[test]
fn state_and_meta_round_trip_over_mcp() {
    let (mut creator, mut joiner, _mesh, _nick) = create_pair(900);

    // state: creator merges, the joiner's get_state reflects it.
    let merged = tool_result_json(&creator.tool_call(
        910,
        "apply_state_merge",
        serde_json::json!({ "merge": {"turn": "a"} }),
    ))
    .expect("apply_state_merge must succeed");
    assert_eq!(merged["ok"], true, "apply_state_merge reports ok");
    assert!(
        poll_doc(&mut joiner, 911, "get_state", |doc| doc["turn"] == "a"),
        "joiner's get_state never reflected the creator's state merge"
    );

    // meta: creator merges, the joiner's get_meta reflects it.
    let meta_merged = tool_result_json(&creator.tool_call(
        920,
        "apply_meta_merge",
        serde_json::json!({ "merge": {"peers": {"creator": {"model": "Opus 4.8"}}} }),
    ))
    .expect("apply_meta_merge must succeed");
    assert_eq!(meta_merged["ok"], true, "apply_meta_merge reports ok");
    assert!(
        poll_doc(&mut joiner, 921, "get_meta", |doc| doc
            .pointer("/peers/creator/model")
            == Some(&serde_json::json!("Opus 4.8"))),
        "joiner's get_meta never reflected the creator's meta merge"
    );

    // Channel isolation: the state key isn't in meta, and the meta key isn't in
    // state.
    let meta_doc = tool_result_json(&joiner.tool_call(930, "get_meta", serde_json::json!({})))
        .expect("get_meta must succeed");
    assert!(
        meta_doc["document"].get("turn").is_none(),
        "a state key leaked into the meta document: {}",
        meta_doc["document"]
    );
    let state_doc = tool_result_json(&joiner.tool_call(931, "get_state", serde_json::json!({})))
        .expect("get_state must succeed");
    assert!(
        state_doc["document"].get("peers").is_none(),
        "a meta key leaked into the state document: {}",
        state_doc["document"]
    );
}
