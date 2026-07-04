use std::time::Duration;

use anyhow::Result;
use interprocess::local_socket::{
    ListenerOptions, Name,
    tokio::{Listener, Stream, prelude::*},
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::a2a::{TaskId, TaskState};
use crate::protocol::{MessageBody, MessageId, Nickname, SwarmId};
use crate::util::bounded_read::{LineRead, read_bounded_line};
use crate::util::consts::{MAX_IPC_COMMAND_BYTES, MAX_IPC_RESPONSE_BYTES, RUNTIME_DIR};
use crate::util::swarm_runtime_dir;
use crate::util::tuning::{
    IPC_ACCEPT_BACKOFF_MAX_SECS, IPC_ACCEPT_BACKOFF_MIN_MS, IPC_IO_TIMEOUT_SECS,
};

/// Returns the IPC endpoint identifier for a specific agent on a swarm —
/// a filesystem socket path (the project targets Unix only). Lives in the
/// swarm's runtime folder beside its `<nick>.tracing.log` / `<nick>.state.json`.
pub(crate) fn socket_path(swarm: &SwarmId, nickname: &Nickname) -> String {
    format!(
        "{}/{nickname}.ipc.sock",
        swarm_runtime_dir(swarm.as_str()).display()
    )
}

fn to_name(path: &str) -> Result<Name<'_>> {
    use interprocess::local_socket::{GenericFilePath, ToFsName};
    Ok(path.to_fs_name::<GenericFilePath>()?)
}

/// Type alias for messages flowing from IPC listener to the event loop.
/// The event loop receives the command and sends back a raw JSON response string.
pub(crate) type IpcMessage = (IpcCommand, tokio::sync::oneshot::Sender<String>);

/// Command sent from CLI to the running server over IPC.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command")]
pub(crate) enum IpcCommand {
    /// Broadcast a swarm chat message (A2A `message/send` with no addressee).
    #[serde(rename = "msg")]
    Msg { swarm: SwarmId, body: MessageBody },
    #[serde(rename = "poll")]
    Poll {
        swarm: SwarmId,
        /// Surfaced-event seq cursor: return events surfaced after this seq.
        /// Omitted on the first poll (returns the buffered history). The
        /// per-event `seq` in the response is the value to pass next.
        #[serde(skip_serializing_if = "Option::is_none")]
        after: Option<u64>,
        /// Long-poll: park this read up to the server cap (`longpoll_max_ms`),
        /// returning early on the first new surfaced event, else `[]` at the
        /// deadline. Absent/false is an immediate read. Skipped when false so
        /// the wire stays byte-stable for callers that never long-poll.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        long: bool,
    },
    /// Arm an RTT round: the daemon broadcasts a ping probe, collects
    /// pongs for a fixed window, and emits a `ping_report` on its
    /// `--output json` stream. Fire-and-forget — the ack is immediate.
    #[serde(rename = "ping")]
    Ping { swarm: SwarmId },
    /// Worker-emit a `TaskStatusUpdate` on a task we're serving (`a2a status`).
    #[serde(rename = "a2a_status")]
    A2aStatus {
        swarm: SwarmId,
        task_id: TaskId,
        #[serde(with = "crate::a2a::friendly_state")]
        state: TaskState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// Worker-emit a `TaskArtifactUpdate` (the result) on a task (`a2a artifact`).
    #[serde(rename = "a2a_artifact")]
    A2aArtifact {
        swarm: SwarmId,
        task_id: TaskId,
        text: String,
    },
    /// Query the live participant roster (nicknames + recency) — backs the
    /// task sender's target picker and nickname validation.
    #[serde(rename = "peers")]
    Peers { swarm: SwarmId },
    /// Apply an RFC 7386 JSON Merge Patch to the swarm's shared state. `merge` is
    /// any JSON value merged into the document (an object deep-merges, `null`
    /// deletes a key, a non-object replaces the target); the daemon signs +
    /// gossips it.
    #[serde(rename = "state_merge")]
    StateMerge {
        swarm: SwarmId,
        merge: serde_json::Value,
    },
    /// Read the current derived shared-state document.
    #[serde(rename = "state_get")]
    StateGet { swarm: SwarmId },
    /// `meta`-channel counterpart of [`StateMerge`](IpcCommand::StateMerge).
    #[serde(rename = "meta_merge")]
    MetaMerge {
        swarm: SwarmId,
        merge: serde_json::Value,
    },
    /// `meta`-channel counterpart of [`StateGet`](IpcCommand::StateGet).
    #[serde(rename = "meta_get")]
    MetaGet { swarm: SwarmId },
    /// A gossip A2A call: send a directed A2A JSON-RPC request to `to` and
    /// return its response (or a timeout error). `to` serves the safe method
    /// set only.
    #[serde(rename = "a2a_call")]
    A2aCall {
        swarm: SwarmId,
        to: Nickname,
        method: String,
        #[serde(default)]
        params: serde_json::Value,
        timeout_secs: u64,
    },
    /// Identity probe for `doctor`: the daemon answers with its own swarm id,
    /// human name, nickname, and participant count. Carries no swarm — a
    /// socket serves exactly one swarm and `doctor` is asking *which*, so it
    /// addresses the daemon by socket path, not by id.
    #[serde(rename = "info")]
    Info,
}

impl IpcCommand {
    /// The swarm a command is addressed to, used to derive the socket path in
    /// [`send`]. `None` for [`IpcCommand::Info`], which is sent by socket path
    /// directly (the daemon answers with its own id).
    pub(crate) fn swarm_id(&self) -> Option<&SwarmId> {
        match self {
            IpcCommand::Msg { swarm, .. }
            | IpcCommand::Poll { swarm, .. }
            | IpcCommand::Ping { swarm }
            | IpcCommand::A2aStatus { swarm, .. }
            | IpcCommand::A2aArtifact { swarm, .. }
            | IpcCommand::Peers { swarm }
            | IpcCommand::StateMerge { swarm, .. }
            | IpcCommand::StateGet { swarm }
            | IpcCommand::MetaMerge { swarm, .. }
            | IpcCommand::MetaGet { swarm }
            | IpcCommand::A2aCall { swarm, .. } => Some(swarm),
            IpcCommand::Info => None,
        }
    }
}

/// Richer `ok` response for the `msg` IPC that also echoes back
/// the authoritative message record. The echo has the same shape
/// `poll` returns per entry — `serde_json::to_value(msg)` — so
/// agents can treat it uniformly with `fetch_messages` results.
///
/// A caller that only reads `response["id"]` gets the id; the MCP server
/// reads the embedded `"message"` field for the full record.
pub(crate) fn json_ok_msg(id: &MessageId, msg: &crate::protocol::Message) -> String {
    serde_json::json!({
        "ok": true,
        "id": id,
        "message": serde_json::to_value(msg).expect("Message serialize is infallible"),
    })
    .to_string()
}

/// Lean `{ok, id}` response — used by IPC commands that don't have
/// a message to echo back (currently test-only).
#[cfg(test)]
pub(crate) fn json_ok(id: &str) -> String {
    serde_json::json!({"ok": true, "id": id}).to_string()
}

/// Bare `{ok:true}` ack for fire-and-forget IPC commands (`ping`) that
/// have no id or payload to return.
pub(crate) fn json_ack() -> String {
    serde_json::json!({"ok": true}).to_string()
}

pub(crate) fn json_error(error: &str) -> String {
    serde_json::json!({"ok": false, "error": error}).to_string()
}

/// Bind the local IPC socket synchronously, returning the listening
/// socket. Done *before* the daemon marks itself ready so that "ready"
/// can never precede an accepting socket: a `ready` gate that observes
/// the readiness flag is then guaranteed a subsequent `connect` succeeds.
///
/// # Errors
/// An invalid socket name, or the OS refusing the bind.
pub(crate) fn bind(swarm: &SwarmId, nickname: &Nickname) -> Result<Listener> {
    let path = socket_path(swarm, nickname);

    // Best-effort cleanup of a stale socket file and (re)create the swarm's
    // runtime folder (the socket's parent).
    let _ = std::fs::remove_file(&path);
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let name = to_name(&path)?;
    let listener = ListenerOptions::new()
        .name(name)
        .create_tokio()
        .map_err(|error| anyhow::anyhow!("failed to bind IPC socket {path}: {error}"))?;
    tracing::info!(?path, "IPC socket listening");
    Ok(listener)
}

/// Server-side accept loop over an already-[`bind`]-ed socket: forward
/// each connection's command to the event loop. Spawned after `bind`
/// returns, so by the time this runs the socket is already accepting.
pub(crate) async fn serve(
    listener: Listener,
    tx: mpsc::Sender<IpcMessage>,
    output: crate::output::Output,
) {
    // Accept errors are retried forever: they are almost always
    // transient (fd exhaustion under load, an aborted handshake), and
    // the old `break` permanently killed msg/poll for the process
    // lifetime on the first one — a silent partial outage on a daemon
    // meant to run for weeks. The backoff keeps a persistently failing
    // listener from spinning; the operator-facing error event fires
    // once per failure streak, not once per retry.
    let mut backoff = Duration::from_millis(IPC_ACCEPT_BACKOFF_MIN_MS);
    let mut failing = false;
    loop {
        match listener.accept().await {
            Ok(stream) => {
                backoff = Duration::from_millis(IPC_ACCEPT_BACKOFF_MIN_MS);
                failing = false;
                let tx = tx.clone();
                tokio::spawn(handle_connection(stream, tx));
            }
            Err(error) => {
                if !failing {
                    output.error(&format!("IPC: accept error (retrying): {error}"));
                    failing = true;
                }
                tracing::warn!(
                    %error,
                    backoff_ms = u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX),
                    "IPC: accept error; retrying"
                );
                tokio::time::sleep(backoff).await;
                backoff = next_accept_backoff(backoff);
            }
        }
    }
}

/// Double the accept backoff up to its cap.
fn next_accept_backoff(current: Duration) -> Duration {
    current
        .saturating_mul(2)
        .min(Duration::from_secs(IPC_ACCEPT_BACKOFF_MAX_SECS))
}

async fn handle_connection(stream: Stream, tx: mpsc::Sender<IpcMessage>) {
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);

    // I/O deadline on both legs: a client that connects and goes silent
    // (or stops draining the response) would otherwise pin this task and
    // its fd for the daemon's lifetime.
    let io_deadline = Duration::from_secs(IPC_IO_TIMEOUT_SECS);
    let line = match tokio::time::timeout(
        io_deadline,
        read_bounded_line(&mut reader, MAX_IPC_COMMAND_BYTES),
    )
    .await
    {
        Ok(Ok(LineRead::Line(line))) => line,
        Ok(Ok(LineRead::TooLong)) => {
            let error = json_error("command too large");
            let _ = tokio::time::timeout(
                io_deadline,
                write_half.write_all(format!("{error}\n").as_bytes()),
            )
            .await;
            return;
        }
        Ok(Ok(LineRead::Eof) | Err(_)) => return,
        Err(_idle) => {
            tracing::debug!("IPC: connection sent nothing within the read deadline; closing");
            return;
        }
    };

    let response = match serde_json::from_str::<IpcCommand>(line.trim()) {
        Err(error) => json_error(&format!("parse error: {error}")),
        Ok(cmd) => {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if tx.send((cmd, resp_tx)).await.is_err() {
                json_error("server channel closed")
            } else {
                match resp_rx.await {
                    Ok(reply) => reply,
                    Err(_) => json_error("response channel dropped"),
                }
            }
        }
    };

    let _ = tokio::time::timeout(
        io_deadline,
        write_half.write_all(format!("{response}\n").as_bytes()),
    )
    .await;
}

/// Client-side: send an IPC command to the running server and return the raw JSON response.
pub(crate) async fn send(cmd: &IpcCommand, nickname: &Nickname) -> Result<String> {
    let swarm = cmd
        .swarm_id()
        .expect("send() is only used for swarm-addressed commands; Info uses send_to_path");
    let path = socket_path(swarm, nickname);
    let name = to_name(&path).map_err(|error| anyhow::anyhow!("invalid socket name: {error}"))?;
    let stream = Stream::connect(name).await.map_err(|_| anyhow::anyhow!(
        "No active swarm server running for nickname '{nickname}'. Start one with `ahsw create` or `ahsw join {{🐝...}} --nickname {nickname}`."
    ))?;
    round_trip(stream, cmd).await
}

/// Client-side: send an IPC command to a specific socket path. `doctor` uses
/// this to query each live daemon discovered under [`RUNTIME_DIR`] — a
/// missing/dead socket is a plain `Err` the caller can skip.
///
/// # Errors
/// The path is not valid UTF-8, the socket can't be connected (no live
/// daemon), or the request/response round trip fails.
pub(crate) async fn send_to_path(path: &std::path::Path, cmd: &IpcCommand) -> Result<String> {
    use anyhow::Context;
    let path_str = path.to_str().context("socket path is not valid UTF-8")?;
    let name =
        to_name(path_str).map_err(|error| anyhow::anyhow!("invalid socket name: {error}"))?;
    let stream = Stream::connect(name)
        .await
        .map_err(|error| anyhow::anyhow!("connect {path_str}: {error}"))?;
    round_trip(stream, cmd).await
}

/// Write `cmd`, half-close, and read back the single-line JSON response.
/// The shared body of [`send`] and [`send_to_path`].
async fn round_trip(stream: Stream, cmd: &IpcCommand) -> Result<String> {
    let (read_half, mut write_half) = tokio::io::split(stream);

    let json = serde_json::to_string(cmd)?;
    write_half.write_all(format!("{json}\n").as_bytes()).await?;
    write_half.shutdown().await?;

    let mut reader = BufReader::new(read_half);
    match read_bounded_line(&mut reader, MAX_IPC_RESPONSE_BYTES).await? {
        LineRead::Line(line) => Ok(line.trim().to_string()),
        LineRead::Eof => Ok(String::new()),
        LineRead::TooLong => anyhow::bail!("IPC response too large"),
    }
}

/// Every live daemon's IPC socket on this machine — one `<nick>.ipc.sock`
/// inside each swarm's folder (`<RUNTIME_DIR>/<swarm-prefix>/`). Best-effort:
/// a missing [`RUNTIME_DIR`] yields an empty list. Drives `doctor`'s
/// active-swarm discovery, so it walks the per-swarm subfolders.
pub(crate) fn active_socket_paths() -> Vec<std::path::PathBuf> {
    let Ok(swarm_dirs) = std::fs::read_dir(RUNTIME_DIR) else {
        return Vec::new();
    };
    swarm_dirs
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .flat_map(|dir| std::fs::read_dir(dir).into_iter().flatten().flatten())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "sock"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        IpcCommand, IpcMessage, MessageBody, Nickname, SwarmId, TaskId, TaskState, bind,
        json_error, json_ok, mpsc, send, serve, socket_path,
    };

    // ── pure functions ─────────────────────────────────────────────

    #[test]
    fn json_ok_is_valid_json() {
        let json = json_ok("abc-123");
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["id"], "abc-123");
    }

    #[test]
    fn json_error_is_valid_json() {
        let json = json_error("something broke");
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["ok"], false);
        assert_eq!(parsed["error"], "something broke");
    }

    #[test]
    fn json_ok_escapes_special_chars() {
        let json = json_ok(r#"id"with"quotes"#);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["id"], r#"id"with"quotes"#);
    }

    #[test]
    fn json_error_escapes_special_chars() {
        let json = json_error(r#"error: "bad input""#);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["error"], r#"error: "bad input""#);
    }

    #[test]
    fn socket_path_format() {
        let path = socket_path(
            &SwarmId::from("🐝abcdefghijkmnpqr"),
            &Nickname::from("my-nick"),
        );
        assert!(path.starts_with("/tmp/agent-habilis/swarm/"));
        assert!(path.ends_with("/my-nick.ipc.sock"));
        assert!(path.contains("🐝abcdefghijkmnpq")); // 16-char swarm folder
    }

    // ── IpcCommand serialization ───────────────────────────────────

    #[test]
    fn ipc_command_msg_round_trip() {
        let cmd = IpcCommand::Msg {
            swarm: SwarmId::from("🐝test"),
            body: MessageBody::from("hello"),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed.swarm_id().expect("Msg is swarm-addressed").as_str(),
            "🐝test"
        );
    }

    #[test]
    fn ipc_command_info_round_trip() {
        let json = serde_json::to_string(&IpcCommand::Info).unwrap();
        assert_eq!(json, r#"{"command":"info"}"#);
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        assert!(parsed.swarm_id().is_none(), "Info carries no swarm");
    }

    #[test]
    fn ipc_command_state_merge_round_trip() {
        let cmd = IpcCommand::StateMerge {
            swarm: SwarmId::from("🐝test"),
            merge: serde_json::json!({"turn": "b"}),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""command":"state_merge""#), "tag: {json}");
        assert!(json.contains(r#""merge""#), "{json}");
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::StateMerge { swarm, merge } => {
                assert_eq!(swarm.as_str(), "🐝test");
                assert_eq!(merge, serde_json::json!({"turn": "b"}));
            }
            IpcCommand::Msg { .. }
            | IpcCommand::Poll { .. }
            | IpcCommand::Ping { .. }
            | IpcCommand::A2aStatus { .. }
            | IpcCommand::A2aArtifact { .. }
            | IpcCommand::Peers { .. }
            | IpcCommand::MetaMerge { .. }
            | IpcCommand::MetaGet { .. }
            | IpcCommand::StateGet { .. }
            | IpcCommand::A2aCall { .. }
            | IpcCommand::Info => panic!("expected StateMerge"),
        }
    }

    #[test]
    fn ipc_command_poll_round_trip() {
        let cmd = IpcCommand::Poll {
            swarm: SwarmId::from("🐝test"),
            after: Some(42),
            long: false,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        // `long: false` is skipped on the wire, keeping the format byte-stable
        // for callers that never long-poll.
        assert!(!json.contains("long"), "false long must not serialize");
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::Poll { after, long, .. } => {
                assert_eq!(after, Some(42));
                assert!(!long, "absent long deserializes to false");
            }
            IpcCommand::Msg { .. }
            | IpcCommand::Ping { .. }
            | IpcCommand::A2aStatus { .. }
            | IpcCommand::A2aArtifact { .. }
            | IpcCommand::Peers { .. }
            | IpcCommand::StateMerge { .. }
            | IpcCommand::MetaMerge { .. }
            | IpcCommand::MetaGet { .. }
            | IpcCommand::StateGet { .. }
            | IpcCommand::A2aCall { .. }
            | IpcCommand::Info => panic!("expected Poll"),
        }
    }

    #[test]
    fn ipc_command_ping_round_trip() {
        let cmd = IpcCommand::Ping {
            swarm: SwarmId::from("🐝test"),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"command\":\"ping\""));
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::Ping { swarm } => assert_eq!(swarm.as_str(), "🐝test"),
            IpcCommand::Msg { .. }
            | IpcCommand::Poll { .. }
            | IpcCommand::A2aStatus { .. }
            | IpcCommand::A2aArtifact { .. }
            | IpcCommand::Peers { .. }
            | IpcCommand::StateMerge { .. }
            | IpcCommand::MetaMerge { .. }
            | IpcCommand::MetaGet { .. }
            | IpcCommand::StateGet { .. }
            | IpcCommand::A2aCall { .. }
            | IpcCommand::Info => panic!("expected Ping"),
        }
    }

    #[test]
    fn ipc_command_a2a_status_round_trip() {
        let cmd = IpcCommand::A2aStatus {
            swarm: SwarmId::from("🐝test"),
            task_id: TaskId::from("550e8400-e29b-41d4-a716-446655440000"),
            state: TaskState::Working,
            note: Some("on it".to_string()),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"command\":\"a2a_status\""));
        assert!(json.contains("\"state\":\"working\""));
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::A2aStatus { state, note, .. } => {
                assert_eq!(state, TaskState::Working);
                assert_eq!(note.as_deref(), Some("on it"));
            }
            IpcCommand::Msg { .. }
            | IpcCommand::Poll { .. }
            | IpcCommand::Ping { .. }
            | IpcCommand::A2aArtifact { .. }
            | IpcCommand::Peers { .. }
            | IpcCommand::StateMerge { .. }
            | IpcCommand::MetaMerge { .. }
            | IpcCommand::MetaGet { .. }
            | IpcCommand::StateGet { .. }
            | IpcCommand::A2aCall { .. }
            | IpcCommand::Info => panic!("expected A2aStatus"),
        }
    }

    #[test]
    fn ipc_command_peers_round_trip() {
        let cmd = IpcCommand::Peers {
            swarm: SwarmId::from("🐝test"),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"command\":\"peers\""));
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::Peers { swarm } => assert_eq!(swarm.as_str(), "🐝test"),
            IpcCommand::Msg { .. }
            | IpcCommand::Poll { .. }
            | IpcCommand::Ping { .. }
            | IpcCommand::A2aStatus { .. }
            | IpcCommand::A2aArtifact { .. }
            | IpcCommand::StateMerge { .. }
            | IpcCommand::MetaMerge { .. }
            | IpcCommand::MetaGet { .. }
            | IpcCommand::StateGet { .. }
            | IpcCommand::A2aCall { .. }
            | IpcCommand::Info => panic!("expected Peers"),
        }
    }

    #[test]
    fn ipc_command_poll_no_after_skips_field() {
        let cmd = IpcCommand::Poll {
            swarm: SwarmId::from("🐝test"),
            after: None,
            long: false,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(!json.contains("after"));
        assert!(!json.contains("long"));
    }

    #[test]
    fn ipc_command_poll_long_serializes_true() {
        let cmd = IpcCommand::Poll {
            swarm: SwarmId::from("🐝test"),
            after: None,
            long: true,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"long\":true"), "wire: {json}");
    }

    // ── property-based tests ───────────────────────────────────────

    mod prop {
        use crate::util::swarm_prefix;
        use proptest::collection::vec as arb_vec;
        use proptest::{prop_assert, prop_assert_eq, proptest, strategy::Strategy};

        use super::{MessageBody, Nickname, SwarmId, json_error, json_ok};

        fn arb_ascii_body() -> impl Strategy<Value = String> {
            arb_vec(0x20u8..0x7Eu8, 0..200).prop_map(|bytes| String::from_utf8(bytes).unwrap())
        }

        fn arb_nickname() -> impl Strategy<Value = String> {
            "[a-z]{3,8}-[a-z]{3,8}"
        }

        fn arb_swarm() -> impl Strategy<Value = SwarmId> {
            "🐝[1-9A-HJ-NP-Za-km-z]{4,24}".prop_map(|raw| SwarmId::new(raw).unwrap())
        }

        proptest! {
            #![proptest_config(crate::proptest_support::config())]
            // ── Round-trip: build_msg_bytes -> Message::parse ──────

            #[test]
            fn prop_build_msg_bytes_message_round_trip(
                swarm in arb_swarm(),
                body in arb_ascii_body(),
                author in arb_nickname(),
            ) {
                let author = Nickname::new(author).unwrap();
                let body = MessageBody::new(body).unwrap();
                let expected_body = body.clone();
                let identity = crate::protocol::identity::Identity::generate();
                let (bytes, built) = crate::protocol::message::build_msg_bytes(
                    &swarm,
                    body,
                    &author,
                    &identity,
                    crate::protocol::message::ChainCtx::genesis(),
                )
                .unwrap();
                prop_assert!(!built.id.as_str().is_empty());
                let parsed = crate::protocol::Message::parse(&bytes).unwrap();
                prop_assert_eq!(&parsed.author, &author);
                prop_assert_eq!(&parsed.body, &expected_body);
                prop_assert_eq!(&parsed.swarm, &swarm);
                prop_assert_eq!(parsed.kind, crate::protocol::MessageKind::A2aMsg);
            }

            // ── JSON response validity ────────────────────────────

            #[test]
            fn prop_json_ok_always_valid(id in arb_ascii_body()) {
                let json = json_ok(&id);
                let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
                prop_assert!(parsed["ok"] == true);
                prop_assert_eq!(&parsed["id"], &id as &str);
            }

            #[test]
            fn prop_json_error_always_valid(msg in arb_ascii_body()) {
                let json = json_error(&msg);
                let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
                prop_assert!(parsed["ok"] == false);
                prop_assert_eq!(&parsed["error"], &msg as &str);
            }

            // ── Injection safety ──────────────────────────────────

            #[test]
            fn prop_json_ok_injection_safe(id in r#"["\\/\n\r\t]{1,50}"#) {
                let json = json_ok(&id);
                let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
                prop_assert_eq!(&parsed["id"], &id as &str);
            }

            #[test]
            fn prop_json_error_injection_safe(msg in r#"["\\/\n\r\t]{1,50}"#) {
                let json = json_error(&msg);
                let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
                prop_assert_eq!(&parsed["error"], &msg as &str);
            }

            // ── Socket prefix ─────────────────────────────────────

            #[test]
            fn prop_swarm_prefix_max_16_chars(swarm in arb_swarm()) {
                let prefix = swarm_prefix(swarm.as_str());
                prop_assert!(prefix.chars().count() <= 16);
            }

            #[test]
            fn prop_swarm_prefix_is_prefix_of_input(swarm in arb_swarm()) {
                let prefix = swarm_prefix(swarm.as_str());
                prop_assert!(swarm.as_str().starts_with(&prefix));
            }
        }
    }

    // ── IPC round-trip via local socket ────────────────────────────

    #[tokio::test]
    async fn ipc_listen_and_send_msg() {
        // Base58-encode the pid so the swarm id passes strict charset validation.
        let pid_b58 = bs58::encode(std::process::id().to_le_bytes()).into_string();
        let swarm = SwarmId::new(format!("🐝ipctest{pid_b58}")).unwrap();
        let nickname = Nickname::from("test-nick");

        let (tx, mut rx) = mpsc::channel::<IpcMessage>(8);

        // Bind synchronously (no sleep needed — the socket is accepting the
        // instant `bind` returns), then spawn the accept loop.
        let listener = bind(&swarm, &nickname).expect("bind IPC socket");
        let listener_handle = tokio::spawn(serve(listener, tx, crate::output::Output::silent()));

        // Spawn a handler that responds to messages
        let handler = tokio::spawn(async move {
            if let Some((cmd, resp_tx)) = rx.recv().await {
                match cmd {
                    IpcCommand::Msg { body, .. } => {
                        let _ = resp_tx.send(json_ok(&format!("got: {body}")));
                    }
                    IpcCommand::Poll { .. }
                    | IpcCommand::Ping { .. }
                    | IpcCommand::A2aStatus { .. }
                    | IpcCommand::A2aArtifact { .. }
                    | IpcCommand::Peers { .. }
                    | IpcCommand::StateMerge { .. }
                    | IpcCommand::MetaMerge { .. }
                    | IpcCommand::MetaGet { .. }
                    | IpcCommand::StateGet { .. }
                    | IpcCommand::A2aCall { .. }
                    | IpcCommand::Info => {
                        let _ = resp_tx.send(json_error("unexpected command"));
                    }
                }
            }
        });

        // Send a command
        let cmd = IpcCommand::Msg {
            swarm: swarm.clone(),
            body: MessageBody::from("test message"),
        };
        let response = send(&cmd, &nickname).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["id"], "got: test message");

        handler.await.unwrap();
        listener_handle.abort();
    }

    #[test]
    fn accept_backoff_doubles_to_cap() {
        use std::time::Duration;

        use crate::util::tuning::{IPC_ACCEPT_BACKOFF_MAX_SECS, IPC_ACCEPT_BACKOFF_MIN_MS};

        let cap = Duration::from_secs(IPC_ACCEPT_BACKOFF_MAX_SECS);
        let mut backoff = Duration::from_millis(IPC_ACCEPT_BACKOFF_MIN_MS);
        let mut previous = backoff;
        for _ in 0..16 {
            backoff = super::next_accept_backoff(backoff);
            assert!(backoff >= previous, "backoff never shrinks");
            assert!(backoff <= cap, "backoff never exceeds the cap");
            previous = backoff;
        }
        assert_eq!(backoff, cap, "sustained failure settles at the cap");
    }

    // An idle client (connects, never sends) must be disconnected at the
    // I/O deadline instead of pinning a handler task + fd for the
    // daemon's lifetime, and the listener must keep serving others
    // throughout. Real-time: waits out `IPC_IO_TIMEOUT_SECS` (10s).
    #[tokio::test]
    async fn idle_connection_is_closed_at_the_read_deadline() {
        use interprocess::local_socket::{
            GenericFilePath, ToFsName, tokio::Stream, tokio::prelude::*,
        };
        use tokio::io::AsyncReadExt;

        let pid_b58 = bs58::encode(std::process::id().to_le_bytes()).into_string();
        let swarm = SwarmId::new(format!("🐝ipcquiet{pid_b58}")).unwrap();
        let nickname = Nickname::from("idle-nick");
        let (tx, mut rx) = mpsc::channel::<IpcMessage>(8);
        let listener = bind(&swarm, &nickname).expect("bind IPC socket");
        let listener_handle = tokio::spawn(serve(listener, tx, crate::output::Output::silent()));
        // Echo handler so a healthy command still round-trips while the
        // idle connection is parked.
        let handler = tokio::spawn(async move {
            while let Some((_cmd, resp_tx)) = rx.recv().await {
                let _ = resp_tx.send(json_ok("healthy"));
            }
        });

        // Park a silent connection.
        let path = socket_path(&swarm, &nickname);
        let name = path.clone().to_fs_name::<GenericFilePath>().unwrap();
        let mut idle = Stream::connect(name).await.unwrap();

        // A healthy command still round-trips while the idle one is parked.
        let cmd = IpcCommand::Ping {
            swarm: swarm.clone(),
        };
        let response = send(&cmd, &nickname).await.unwrap();
        assert!(response.contains("healthy"), "listener stalled: {response}");

        // The parked connection is closed (EOF) at the deadline, with margin.
        let mut sink = Vec::new();
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(crate::util::tuning::IPC_IO_TIMEOUT_SECS + 5),
            idle.read_to_end(&mut sink),
        )
        .await;
        assert!(
            matches!(read, Ok(Ok(0))),
            "idle connection was not closed at the read deadline: {read:?}"
        );

        handler.abort();
        listener_handle.abort();
    }
}
