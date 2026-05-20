use anyhow::Result;
use interprocess::local_socket::{
    ListenerOptions, Name,
    tokio::{Listener, Stream, prelude::*},
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::protocol::{MessageBody, MessageId, Nickname, SwarmId};
use crate::util::tuning::{TMP_DIR, log_dir};

fn swarm_prefix(swarm: &SwarmId) -> String {
    swarm.as_str().chars().take(16).collect()
}

/// Returns the IPC endpoint identifier for a specific agent on a swarm.
/// On Unix this is a filesystem socket path; on Windows the filename portion
/// becomes a namespaced named-pipe name.
pub(crate) fn socket_path(swarm: &SwarmId, nickname: &Nickname) -> String {
    format!("{}/{}-{}.sock", TMP_DIR, swarm_prefix(swarm), nickname)
}

/// Per-member log file — same `<swarm_prefix>-<nick>` stem as the
/// socket, so logs and sockets never drift. Dir is `AHS_LOG_DIR` if
/// set (tests isolate there), else `{TMP_DIR}/logs`.
pub(crate) fn log_file_path(swarm: &SwarmId, nickname: &Nickname) -> std::path::PathBuf {
    std::path::PathBuf::from(format!(
        "{}/{}-{}.log",
        log_dir(),
        swarm_prefix(swarm),
        nickname
    ))
}

#[cfg(unix)]
fn to_name(path: &str) -> Result<Name<'_>> {
    use interprocess::local_socket::{GenericFilePath, ToFsName};
    Ok(path.to_fs_name::<GenericFilePath>()?)
}

#[cfg(windows)]
fn to_name(path: &str) -> Result<Name<'_>> {
    use interprocess::local_socket::{GenericNamespaced, ToNsName};
    let name = std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path);
    Ok(name.to_ns_name::<GenericNamespaced>()?)
}

/// Type alias for messages flowing from IPC listener to the event loop.
/// The event loop receives the command and sends back a raw JSON response string.
pub(crate) type IpcMessage = (IpcCommand, tokio::sync::oneshot::Sender<String>);

/// Command sent from CLI to the running server over IPC.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command")]
pub(crate) enum IpcCommand {
    #[serde(rename = "msg")]
    Msg {
        swarm: SwarmId,
        body: MessageBody,
        #[serde(skip_serializing_if = "Option::is_none")]
        reply: Option<Nickname>,
    },
    #[serde(rename = "poll")]
    Poll {
        swarm: SwarmId,
        #[serde(skip_serializing_if = "Option::is_none")]
        after: Option<MessageId>,
    },
}

impl IpcCommand {
    pub(crate) fn swarm_id(&self) -> &SwarmId {
        match self {
            IpcCommand::Msg { swarm, .. } | IpcCommand::Poll { swarm, .. } => swarm,
        }
    }
}

/// Richer `ok` response for the `msg` IPC that also echoes back
/// the authoritative message record. The echo has the same shape
/// `poll` returns per entry — `serde_json::to_value(msg)` — so
/// agents can treat it uniformly with `fetch_messages` results.
///
/// Legacy CLI callers that only read `response["id"]` keep working;
/// the MCP server reads the embedded `"message"` field.
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

pub(crate) fn json_error(error: &str) -> String {
    serde_json::json!({"ok": false, "error": error}).to_string()
}

/// Server-side: listen on the local socket and forward commands to the event loop.
pub(crate) async fn listen(
    swarm: SwarmId,
    nickname: Nickname,
    tx: mpsc::Sender<IpcMessage>,
    output: crate::output::Output,
) {
    let path = socket_path(&swarm, &nickname);

    // Best-effort cleanup of a stale socket file and parent dir (Unix only —
    // Windows named pipes are kernel objects with no filesystem presence).
    #[cfg(unix)]
    {
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::create_dir_all(TMP_DIR);
    }

    let name = match to_name(&path) {
        Ok(socket_name) => socket_name,
        Err(error) => {
            output.error(&format!("IPC: invalid socket name: {error}"));
            tracing::warn!(%error, "IPC: invalid socket name");
            return;
        }
    };

    let listener: Listener = match ListenerOptions::new().name(name).create_tokio() {
        Ok(bound) => bound,
        Err(error) => {
            output.error(&format!("IPC: failed to bind socket: {error}"));
            tracing::warn!(%error, "IPC: failed to bind socket");
            return;
        }
    };
    tracing::info!(?path, "IPC socket listening");

    loop {
        match listener.accept().await {
            Ok(stream) => {
                let tx = tx.clone();
                tokio::spawn(handle_connection(stream, tx));
            }
            Err(error) => {
                output.error(&format!("IPC: accept error: {error}"));
                tracing::warn!(%error, "IPC: accept error");
                break;
            }
        }
    }
}

async fn handle_connection(stream: Stream, tx: mpsc::Sender<IpcMessage>) {
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    if reader.read_line(&mut line).await.is_err() {
        return;
    }

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

    let _ = write_half
        .write_all(format!("{response}\n").as_bytes())
        .await;
}

/// Truncate the swarm identifier to its first 16 characters.
/// Exposed for testing.
#[cfg(test)]
pub(crate) fn test_swarm_prefix(swarm: &SwarmId) -> String {
    swarm_prefix(swarm)
}

/// Client-side: send an IPC command to the running server and return the raw JSON response.
pub(crate) async fn send(cmd: &IpcCommand, nickname: &Nickname) -> Result<String> {
    let path = socket_path(cmd.swarm_id(), nickname);
    let name = to_name(&path).map_err(|error| anyhow::anyhow!("invalid socket name: {error}"))?;
    let stream = Stream::connect(name).await.map_err(|_| anyhow::anyhow!(
        "No active swarm server running for nickname '{nickname}'. Start one with `ahs create` or `ahs join {{ahs...}} --nickname {nickname}`."
    ))?;
    let (read_half, mut write_half) = tokio::io::split(stream);

    let json = serde_json::to_string(cmd)?;
    write_half.write_all(format!("{json}\n").as_bytes()).await?;
    write_half.shutdown().await?;

    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    reader.read_line(&mut line).await?;

    Ok(line.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn swarm_prefix_truncates_to_16() {
        assert_eq!(
            test_swarm_prefix(&SwarmId::from("ahsabcdefghijkmnpqrs")).len(),
            16
        );
    }

    #[test]
    fn swarm_prefix_short_input_unchanged() {
        assert_eq!(
            test_swarm_prefix(&SwarmId::from("ahsabcd")).as_str(),
            "ahsabcd"
        );
    }

    #[test]
    fn socket_path_format() {
        let path = socket_path(
            &SwarmId::from("ahsabcdefghijkmnpqr"),
            &Nickname::from("my-nick"),
        );
        assert!(path.starts_with("/tmp/agent-habilis-swarm/"));
        assert!(path.ends_with("-my-nick.sock"));
        assert!(path.contains("ahsabcdefghijkmn")); // 16 chars
    }

    // ── IpcCommand serialization ───────────────────────────────────

    #[test]
    fn ipc_command_msg_round_trip() {
        let cmd = IpcCommand::Msg {
            swarm: SwarmId::from("ahstest"),
            body: MessageBody::from("hello"),
            reply: None,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.swarm_id().as_str(), "ahstest");
    }

    #[test]
    fn ipc_command_msg_with_reply_target() {
        let target = Nickname::from("alice");
        let cmd = IpcCommand::Msg {
            swarm: SwarmId::from("ahstest"),
            body: MessageBody::from("reply"),
            reply: Some(target.clone()),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("alice"));
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::Msg { reply, .. } => assert_eq!(reply, Some(target)),
            IpcCommand::Poll { .. } => panic!("expected Msg"),
        }
    }

    #[test]
    fn ipc_command_poll_round_trip() {
        let id = MessageId::from("550e8400-e29b-41d4-a716-446655440000");
        let cmd = IpcCommand::Poll {
            swarm: SwarmId::from("ahstest"),
            after: Some(id.clone()),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::Poll { after, .. } => assert_eq!(after, Some(id)),
            IpcCommand::Msg { .. } => panic!("expected Poll"),
        }
    }

    #[test]
    fn ipc_command_poll_no_after_skips_field() {
        let cmd = IpcCommand::Poll {
            swarm: SwarmId::from("ahstest"),
            after: None,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(!json.contains("after"));
    }

    // ── property-based tests ───────────────────────────────────────

    mod prop {
        use super::*;
        use proptest::collection::vec as arb_vec;
        use proptest::prelude::*;

        fn arb_ascii_body() -> impl Strategy<Value = String> {
            arb_vec(0x20u8..0x7Eu8, 0..200).prop_map(|bytes| String::from_utf8(bytes).unwrap())
        }

        fn arb_nickname() -> impl Strategy<Value = String> {
            "[a-z]{3,8}-[a-z]{3,8}"
        }

        fn arb_swarm() -> impl Strategy<Value = SwarmId> {
            "ahs[1-9A-HJ-NP-Za-km-z]{4,24}".prop_map(|raw| SwarmId::new(raw).unwrap())
        }

        proptest! {
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
                let (bytes, built) =
                    crate::protocol::message::build_msg_bytes(&swarm, body, None, &author).unwrap();
                prop_assert!(!built.id.as_str().is_empty());
                let parsed = crate::protocol::Message::parse(&bytes).unwrap();
                prop_assert_eq!(&parsed.author, &author);
                prop_assert_eq!(&parsed.body, &expected_body);
                prop_assert_eq!(&parsed.swarm, &swarm);
                prop_assert_eq!(parsed.kind, crate::protocol::MessageKind::Msg { reply: None });
            }

            #[test]
            fn prop_build_msg_bytes_reply_round_trip(
                swarm in arb_swarm(),
                body in arb_ascii_body(),
                author in arb_nickname(),
                target in arb_nickname(),
            ) {
                let author = Nickname::new(author).unwrap();
                let target = Nickname::new(target).unwrap();
                let body = MessageBody::new(body).unwrap();
                let expected_body = body.clone();
                let expected_target = target.clone();
                let (bytes, built) =
                    crate::protocol::message::build_msg_bytes(&swarm, body, Some(target), &author)
                        .unwrap();
                prop_assert!(!built.id.as_str().is_empty());
                let parsed = crate::protocol::Message::parse(&bytes).unwrap();
                prop_assert_eq!(&parsed.author, &author);
                prop_assert_eq!(&parsed.body, &expected_body);
                prop_assert_eq!(&parsed.swarm, &swarm);
                prop_assert_eq!(
                    parsed.kind,
                    crate::protocol::MessageKind::Msg { reply: Some(expected_target) }
                );
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
                let prefix = test_swarm_prefix(&swarm);
                prop_assert!(prefix.chars().count() <= 16);
            }

            #[test]
            fn prop_swarm_prefix_is_prefix_of_input(swarm in arb_swarm()) {
                let prefix = test_swarm_prefix(&swarm);
                prop_assert!(swarm.as_str().starts_with(&prefix));
            }
        }
    }

    // ── IPC round-trip via local socket ────────────────────────────

    #[tokio::test]
    async fn ipc_listen_and_send_msg() {
        // Base58-encode the pid so the swarm id passes strict charset validation.
        let pid_b58 = bs58::encode(std::process::id().to_le_bytes()).into_string();
        let swarm = SwarmId::new(format!("ahsipctest{pid_b58}")).unwrap();
        let nickname = Nickname::from("test-nick");

        let (tx, mut rx) = mpsc::channel::<IpcMessage>(8);

        // Start listener in background
        let swarm_clone = swarm.clone();
        let nickname_clone = nickname.clone();
        let listener_handle = tokio::spawn(async move {
            listen(
                swarm_clone,
                nickname_clone,
                tx,
                crate::output::Output::silent(),
            )
            .await;
        });

        // Give the listener a moment to bind.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Spawn a handler that responds to messages
        let handler = tokio::spawn(async move {
            if let Some((cmd, resp_tx)) = rx.recv().await {
                match cmd {
                    IpcCommand::Msg { body, .. } => {
                        let _ = resp_tx.send(json_ok(&format!("got: {body}")));
                    }
                    IpcCommand::Poll { .. } => {
                        let _ = resp_tx.send(json_error("unexpected command"));
                    }
                }
            }
        });

        // Send a command
        let cmd = IpcCommand::Msg {
            swarm: swarm.clone(),
            body: MessageBody::from("test message"),
            reply: None,
        };
        let response = send(&cmd, &nickname).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["id"], "got: test message");

        handler.await.unwrap();
        listener_handle.abort();
    }
}
