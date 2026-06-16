use std::time::Duration;

use anyhow::Result;
use interprocess::local_socket::{
    ListenerOptions, Name,
    tokio::{Listener, Stream, prelude::*},
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::protocol::{MessageBody, MessageId, Nickname, SwarmId, TaskId, TaskKind, TaskPhase};
use crate::util::bounded_read::{LineRead, read_bounded_line};
use crate::util::consts::{MAX_IPC_COMMAND_BYTES, MAX_IPC_RESPONSE_BYTES, SOCKET_DIR};
use crate::util::swarm_prefix;
use crate::util::tuning::{
    IPC_ACCEPT_BACKOFF_MAX_SECS, IPC_ACCEPT_BACKOFF_MIN_MS, IPC_IO_TIMEOUT_SECS,
};

/// Returns the IPC endpoint identifier for a specific agent on a swarm —
/// a filesystem socket path (the project targets Unix only).
pub(crate) fn socket_path(swarm: &SwarmId, nickname: &Nickname) -> String {
    format!(
        "{SOCKET_DIR}/{}-{}.sock",
        swarm_prefix(swarm.as_str()),
        nickname
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
    /// Arm an RTT round: the daemon broadcasts a ping probe, collects
    /// pongs for a fixed window, and emits a `ping_report` on its
    /// `--output json` stream. Fire-and-forget — the ack is immediate.
    #[serde(rename = "ping")]
    Ping { swarm: SwarmId },
    /// Send one leg of a task exchange to `to`, correlated by `task_id`.
    /// `Offer` carries the task brief; later phases the Q&A / progress /
    /// outcome. The daemon validates `to` against the live roster for
    /// `Offer` only.
    #[serde(rename = "task")]
    Task {
        swarm: SwarmId,
        to: Nickname,
        task_id: TaskId,
        kind: TaskKind,
        phase: TaskPhase,
        body: MessageBody,
    },
    /// Query the live participant roster (nicknames + recency) — backs the
    /// task sender's target picker and nickname validation.
    #[serde(rename = "peers")]
    Peers { swarm: SwarmId },
}

impl IpcCommand {
    pub(crate) fn swarm_id(&self) -> &SwarmId {
        match self {
            IpcCommand::Msg { swarm, .. }
            | IpcCommand::Poll { swarm, .. }
            | IpcCommand::Ping { swarm }
            | IpcCommand::Task { swarm, .. }
            | IpcCommand::Peers { swarm } => swarm,
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

/// Response for a send dropped by the sender-side rate limiter. `ok:false`
/// keeps error-only readers from treating it as a success; the
/// `rate_limited` flag lets aware callers tell it apart from a real error
/// (it is a deliberate drop, not a failure).
pub(crate) fn json_rate_limited() -> String {
    serde_json::json!({"ok": false, "rate_limited": true}).to_string()
}

/// Server-side: listen on the local socket and forward commands to the event loop.
pub(crate) async fn listen(
    swarm: SwarmId,
    nickname: Nickname,
    tx: mpsc::Sender<IpcMessage>,
    output: crate::output::Output,
) {
    let path = socket_path(&swarm, &nickname);

    // Best-effort cleanup of a stale socket file and (re)create the parent dir.
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::create_dir_all(SOCKET_DIR);

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
    let path = socket_path(cmd.swarm_id(), nickname);
    let name = to_name(&path).map_err(|error| anyhow::anyhow!("invalid socket name: {error}"))?;
    let stream = Stream::connect(name).await.map_err(|_| anyhow::anyhow!(
        "No active swarm server running for nickname '{nickname}'. Start one with `ah-s create` or `ah-s join {{ahs...}} --nickname {nickname}`."
    ))?;
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

#[cfg(test)]
mod tests {
    use super::{
        IpcCommand, IpcMessage, MessageBody, MessageId, Nickname, SwarmId, TaskId, TaskKind,
        TaskPhase, json_error, json_ok, listen, mpsc, send, socket_path,
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
            &SwarmId::from("ahsabcdefghijkmnpqr"),
            &Nickname::from("my-nick"),
        );
        assert!(path.starts_with("/tmp/agent-habilis/swarm/sockets/"));
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
            IpcCommand::Poll { .. }
            | IpcCommand::Ping { .. }
            | IpcCommand::Task { .. }
            | IpcCommand::Peers { .. } => panic!("expected Msg"),
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
            IpcCommand::Msg { .. }
            | IpcCommand::Ping { .. }
            | IpcCommand::Task { .. }
            | IpcCommand::Peers { .. } => panic!("expected Poll"),
        }
    }

    #[test]
    fn ipc_command_ping_round_trip() {
        let cmd = IpcCommand::Ping {
            swarm: SwarmId::from("ahstest"),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"command\":\"ping\""));
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::Ping { swarm } => assert_eq!(swarm.as_str(), "ahstest"),
            IpcCommand::Msg { .. }
            | IpcCommand::Poll { .. }
            | IpcCommand::Task { .. }
            | IpcCommand::Peers { .. } => panic!("expected Ping"),
        }
    }

    #[test]
    fn ipc_command_task_round_trip() {
        let cmd = IpcCommand::Task {
            swarm: SwarmId::from("ahstest"),
            to: Nickname::from("calm-otter"),
            task_id: TaskId::from("550e8400-e29b-41d4-a716-446655440000"),
            kind: TaskKind::Handover,
            phase: TaskPhase::Offer,
            body: MessageBody::from("## Task\nport the parser"),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"command\":\"task\""));
        assert!(json.contains("\"kind\":\"handover\""));
        assert!(json.contains("\"phase\":\"offer\""));
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::Task {
                to, kind, phase, ..
            } => {
                assert_eq!(to, Nickname::from("calm-otter"));
                assert_eq!(kind, TaskKind::Handover);
                assert_eq!(phase, TaskPhase::Offer);
            }
            IpcCommand::Msg { .. }
            | IpcCommand::Poll { .. }
            | IpcCommand::Ping { .. }
            | IpcCommand::Peers { .. } => panic!("expected Task"),
        }
    }

    #[test]
    fn ipc_command_peers_round_trip() {
        let cmd = IpcCommand::Peers {
            swarm: SwarmId::from("ahstest"),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"command\":\"peers\""));
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::Peers { swarm } => assert_eq!(swarm.as_str(), "ahstest"),
            IpcCommand::Msg { .. }
            | IpcCommand::Poll { .. }
            | IpcCommand::Ping { .. }
            | IpcCommand::Task { .. } => panic!("expected Peers"),
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
            "ahs[1-9A-HJ-NP-Za-km-z]{4,24}".prop_map(|raw| SwarmId::new(raw).unwrap())
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
                    None,
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
                let identity = crate::protocol::identity::Identity::generate();
                let (bytes, built) = crate::protocol::message::build_msg_bytes(
                    &swarm,
                    body,
                    Some(target),
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
                    IpcCommand::Poll { .. }
                    | IpcCommand::Ping { .. }
                    | IpcCommand::Task { .. }
                    | IpcCommand::Peers { .. } => {
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
        let swarm = SwarmId::new(format!("ahsipcquiet{pid_b58}")).unwrap();
        let nickname = Nickname::from("idle-nick");
        let (tx, mut rx) = mpsc::channel::<IpcMessage>(8);
        let listener_handle = tokio::spawn(listen(
            swarm.clone(),
            nickname.clone(),
            tx,
            crate::output::Output::silent(),
        ));
        // Echo handler so a healthy command still round-trips while the
        // idle connection is parked.
        let handler = tokio::spawn(async move {
            while let Some((_cmd, resp_tx)) = rx.recv().await {
                let _ = resp_tx.send(json_ok("healthy"));
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

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
