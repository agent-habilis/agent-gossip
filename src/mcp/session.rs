//! Holds one active swarm's event loop + command channel for the MCP server.

use std::sync::Mutex;

use anyhow::{Context, Result, anyhow};
use iroh::RelayUrl;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::daemon::{
    self, DriverMode,
    setup::{SetupKind, setup_swarm},
};
use crate::protocol::swarm::{DiscoveryOpts, Swarm, SwarmMode, SwarmName};
use crate::protocol::{MessageBody, MessageId, Nickname, SwarmId};
use crate::transport::ipc::{IpcCommand, IpcMessage};

/// A running swarm with an open command channel. Owned by the MCP
/// server. Dropping this aborts the event loop task.
pub(super) struct Session {
    pub swarm: SwarmId,
    pub name: String,
    pub nickname: Nickname,
    cmd_tx: mpsc::Sender<IpcMessage>,
    quit_tx: mpsc::Sender<()>,
    /// Implicit `after` cursor.
    last_delivered_id: Mutex<Option<MessageId>>,
    /// `None` after `leave()`; aborted in `Drop` otherwise.
    task: Option<JoinHandle<Result<()>>>,
}

impl Session {
    /// Start a new swarm (Create) and its event loop.
    pub(super) async fn create(
        mode: SwarmMode,
        name: SwarmName,
        relay: Option<RelayUrl>,
        nickname: Nickname,
    ) -> Result<Self> {
        let label = name.as_str().to_string();
        let discovery = DiscoveryOpts::legacy(mode, relay);
        spawn_session(SetupKind::Create { mode, name }, discovery, label, nickname).await
    }

    /// Join an existing swarm. Resolves `swarm_input` via the normal
    /// resolver (ahs…, domain, git URL).
    pub(super) async fn join(swarm_input: &str, nickname: Nickname) -> Result<Self> {
        let swarm: Swarm = crate::resolver::resolve(swarm_input).await?;
        let label = swarm.name.as_str().to_string();
        let discovery = DiscoveryOpts::legacy(swarm.mode, None);
        spawn_session(SetupKind::Join { swarm }, discovery, label, nickname).await
    }

    async fn send_cmd(&self, cmd: IpcCommand) -> Result<Value> {
        let (resp_tx, resp_rx) = oneshot::channel::<String>();
        self.cmd_tx
            .send((cmd, resp_tx))
            .await
            .map_err(|_| anyhow!("event loop channel closed"))?;
        let response = resp_rx
            .await
            .map_err(|_| anyhow!("event loop dropped response channel"))?;
        serde_json::from_str::<Value>(&response)
            .context("event loop returned invalid JSON response")
    }

    /// Broadcast a message. Returns `Some((id, echo))` where `echo` is the
    /// full authoritative record the daemon built — same shape
    /// `fetch_messages` returns — or `None` when the sender-side rate
    /// limiter dropped it (a deliberate drop, not an error).
    pub(super) async fn send_message(
        &self,
        body: MessageBody,
        reply: Option<Nickname>,
    ) -> Result<Option<(MessageId, Value)>> {
        let cmd = IpcCommand::Msg {
            swarm: self.swarm.clone(),
            body,
            reply,
        };
        // Move-destructure the response so the echo object can be
        // handed back without a deep clone.
        let mut obj = match self.send_cmd(cmd).await? {
            Value::Object(map) => map,
            other @ (Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Array(_)) => return Err(anyhow!("malformed IPC response: {other}")),
        };
        if obj
            .remove("rate_limited")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        {
            return Ok(None);
        }
        match obj.remove("ok").and_then(|value| value.as_bool()) {
            Some(true) => {
                let id = match obj.remove("id") {
                    Some(Value::String(raw)) => MessageId::new(raw).map_err(|error| {
                        anyhow!("msg response 'id' is not a valid MessageId: {error}")
                    })?,
                    _ => return Err(anyhow!("msg response missing 'id'")),
                };
                let echo = obj
                    .remove("message")
                    .ok_or_else(|| anyhow!("msg response missing 'message'"))?;
                self.advance_cursor_to(id.clone());
                Ok(Some((id, echo)))
            }
            Some(false) => {
                let err = obj
                    .remove("error")
                    .and_then(|value| match value {
                        Value::String(text) => Some(text),
                        Value::Null
                        | Value::Bool(_)
                        | Value::Number(_)
                        | Value::Array(_)
                        | Value::Object(_) => None,
                    })
                    .unwrap_or_else(|| "unknown error".to_string());
                Err(anyhow!("send_message failed: {err}"))
            }
            None => Err(anyhow!("malformed IPC response: missing 'ok'")),
        }
    }

    /// Explicit cursor wins; otherwise fall back to the implicit one.
    fn effective_after(&self, explicit: Option<MessageId>) -> Option<MessageId> {
        explicit.or_else(|| self.last_delivered_id.lock().unwrap().clone())
    }

    fn advance_cursor_to(&self, id: MessageId) {
        *self.last_delivered_id.lock().unwrap() = Some(id);
    }

    /// Fetch buffered messages after `after` (or all buffered when
    /// `None` AND no implicit cursor is set yet). Auto-advances the
    /// session's implicit cursor and returns the advanced id so the
    /// caller can surface it without re-scanning the batch.
    pub(super) async fn fetch_messages(
        &self,
        after: Option<MessageId>,
    ) -> Result<(Vec<Value>, Option<MessageId>)> {
        let cmd = IpcCommand::Poll {
            swarm: self.swarm.clone(),
            after: self.effective_after(after),
        };

        let msgs = match self.send_cmd(cmd).await? {
            Value::Array(array) => array,
            other @ (Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Object(_)) => return Err(anyhow!("poll response was not an array: {other}")),
        };
        let current_id = msgs
            .last()
            .and_then(|msg| msg.get("id"))
            .and_then(|value| value.as_str())
            .and_then(|raw| MessageId::new(raw).ok());
        if let Some(id) = current_id.clone() {
            self.advance_cursor_to(id);
        }
        Ok((msgs, current_id))
    }

    /// Clean shutdown: signal the event loop and wait up to 3s for it
    /// to emit `Left` and wind down. If it doesn't, `Drop` aborts the
    /// task.
    pub(super) async fn leave(mut self) {
        let _ = self.quit_tx.send(()).await;
        if let Some(task) = self.task.take() {
            let timeout = tokio::time::sleep(std::time::Duration::from_secs(3));
            tokio::select! {
                _ = task => {}
                () = timeout => {}
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Fallback — if the MCP server exits without calling leave(),
        // still abort the event loop task so we don't leak it.
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn spawn_session(
    kind: SetupKind,
    discovery: DiscoveryOpts,
    name: String,
    nickname: Nickname,
) -> Result<Session> {
    let (cmd_tx, cmd_rx) = mpsc::channel::<IpcMessage>(32);
    let (quit_tx, quit_rx) = mpsc::channel::<()>(1);

    let mut cfg = setup_swarm(
        kind,
        nickname,
        /* interactive */ false,
        crate::util::tuning::DEFAULT_MAX_DIRECT_PEERS,
        /* state_file */ None,
        discovery,
        // MCP owns stdout for JSON-RPC; never print swarm output.
        crate::output::Output::silent(),
    )
    .await
    .context("setup_swarm failed")?;
    cfg.driver = DriverMode::Mcp {
        ipc_rx: cmd_rx,
        quit_rx,
    };

    let swarm = cfg.swarm.clone();
    let session_nickname = cfg.author.clone();
    let task = tokio::spawn(daemon::run(cfg));
    Ok(Session {
        swarm,
        name,
        nickname: session_nickname,
        cmd_tx,
        quit_tx,
        last_delivered_id: Mutex::new(None),
        task: Some(task),
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{MessageBody, MessageId, Nickname, Session, SwarmMode, SwarmName, Value};

    // All tests use the private network (loopback) so they work on
    // any CI without public iroh DNS / relay access.

    async fn wait_for_gossip(session: &Session, author: &str, body: &str) -> Option<MessageId> {
        // Poll up to ~10 s for the message to propagate via gossip.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            if let Ok((msgs, _)) = session.fetch_messages(None).await {
                for entry in &msgs {
                    if entry.get("author").and_then(|value| value.as_str()) == Some(author)
                        && entry.get("body").and_then(|value| value.as_str()) == Some(body)
                    {
                        return entry
                            .get("id")
                            .and_then(|value| value.as_str())
                            .and_then(|raw| MessageId::new(raw).ok());
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        None
    }

    #[tokio::test]
    async fn create_session_yields_valid_swarm_and_nickname() {
        let session = Session::create(
            SwarmMode::Private,
            SwarmName::new("test1").unwrap(),
            None,
            Nickname::from("alice-test"),
        )
        .await
        .expect("create");
        assert!(session.swarm.as_str().starts_with("ahs"));
        assert_eq!(session.name, "test1");
        assert_eq!(session.nickname.as_str(), "alice-test");
        session.leave().await;
    }

    #[tokio::test]
    async fn two_sessions_same_swarm_exchange_messages() {
        let creator = Session::create(
            SwarmMode::Private,
            SwarmName::new("two").unwrap(),
            None,
            Nickname::from("alice-two"),
        )
        .await
        .expect("create");
        let swarm = creator.swarm.clone();

        let joiner = Session::join(swarm.as_str(), Nickname::from("bob-two"))
            .await
            .expect("join");
        assert_eq!(joiner.name, "two");

        // Send from creator → joiner should see it.
        let (sent_id, _) = creator
            .send_message(MessageBody::from("hi bob"), None)
            .await
            .expect("send_message")
            .expect("within rate limit");

        let observed = wait_for_gossip(&joiner, "alice-two", "hi bob").await;
        assert_eq!(
            observed,
            Some(sent_id),
            "joiner should receive message body=hi bob from alice-two with matching id"
        );

        // And the reverse direction.
        let (reply_id, _) = joiner
            .send_message(MessageBody::from("hi alice"), None)
            .await
            .expect("send_message reply")
            .expect("within rate limit");
        let observed2 = wait_for_gossip(&creator, "bob-two", "hi alice").await;
        assert_eq!(observed2, Some(reply_id));

        joiner.leave().await;
        creator.leave().await;
    }

    #[tokio::test]
    async fn send_message_returns_full_echo_and_advances_cursor() {
        // send_message returns an authoritative echo (id, author,
        // ts, body) so callers don't need to re-fetch to see their
        // own send, and advances the cursor past it so subsequent
        // idle fetches don't surface a self-echo feedback loop.
        let alice = Session::create(
            SwarmMode::Private,
            SwarmName::new("replay").unwrap(),
            None,
            Nickname::from("alice-replay"),
        )
        .await
        .expect("create");

        let (sent, echo) = alice
            .send_message(MessageBody::from("self-echo"), None)
            .await
            .expect("send_message")
            .expect("within rate limit");
        assert_eq!(echo["id"].as_str(), Some(sent.as_str()));
        assert_eq!(echo["author"].as_str(), Some("alice-replay"));
        assert_eq!(echo["body"].as_str(), Some("self-echo"));
        assert!(echo["ts"].is_i64(), "echo must carry a numeric ts");

        // Cursor advanced past self-send → default fetch hides it.
        let (msgs, _) = alice.fetch_messages(None).await.expect("fetch");
        let has_own = msgs
            .iter()
            .any(|msg| msg.get("id").and_then(|value| value.as_str()) == Some(sent.as_str()));
        assert!(
            !has_own,
            "implicit cursor should have advanced past self-send, got {msgs:?}"
        );

        alice.leave().await;
    }

    #[tokio::test]
    async fn implicit_cursor_returns_delta_on_subsequent_fetches() {
        // First `fetch_messages(None)` sees full history. Subsequent
        // `fetch_messages(None)` calls see only what arrived since
        // the last one. Explicit `after` still overrides the cursor.
        let alice = Session::create(
            SwarmMode::Private,
            SwarmName::new("cursor").unwrap(),
            None,
            Nickname::from("alice-cursor"),
        )
        .await
        .expect("create");
        let swarm = alice.swarm.clone();
        let bob = Session::join(swarm.as_str(), Nickname::from("bob-cursor"))
            .await
            .expect("join");

        // Drive alice's first cursor-less fetch until bob's join
        // presence lands. That first non-empty fetch advances the
        // implicit cursor past everything currently buffered —
        // which is exactly what we want to test against next.
        let mut first: Vec<Value> = Vec::new();
        let mut expected_cursor: Option<MessageId> = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            let (msgs, cur) = alice.fetch_messages(None).await.expect("first fetch");
            if msgs.iter().any(|msg| {
                msg.get("subtype").and_then(|value| value.as_str()) == Some("joined")
                    && msg.get("author").and_then(|value| value.as_str()) == Some("bob-cursor")
            }) {
                first = msgs;
                expected_cursor = cur;
                break;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        assert!(
            !first.is_empty(),
            "first fetch should eventually see bob's join presence"
        );
        let expected_cursor = expected_cursor.expect("first fetch must have ids");

        // Second fetch with no new traffic: cursor advanced to
        // `expected_cursor`, so the delta must be empty.
        let (empty_delta, _) = alice.fetch_messages(None).await.expect("delta fetch");
        assert!(
            empty_delta.is_empty(),
            "second cursor-less fetch must return delta (empty), got {empty_delta:?}"
        );

        // Bob sends — alice's next cursor-less fetch must surface
        // exactly that message, nothing older.
        bob.send_message(MessageBody::from("hi via cursor"), None)
            .await
            .expect("send");
        let mut saw: Vec<Value> = Vec::new();
        let delta_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < delta_deadline && saw.is_empty() {
            let (msgs, _) = alice.fetch_messages(None).await.expect("delta fetch 2");
            saw = msgs;
            if saw.is_empty() {
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        }
        assert_eq!(
            saw.len(),
            1,
            "delta fetch after bob's send should contain exactly one message, got {saw:?}"
        );
        assert_eq!(saw[0]["body"].as_str(), Some("hi via cursor"));

        // Explicit `after` must override the implicit cursor: pass
        // the cursor position we observed earlier and confirm we
        // still get bob's message back.
        let (forced, _) = alice
            .fetch_messages(Some(expected_cursor))
            .await
            .expect("explicit fetch");
        assert!(
            forced.iter().any(
                |msg| msg.get("body").and_then(|value| value.as_str()) == Some("hi via cursor")
            ),
            "explicit after must override implicit cursor"
        );

        alice.leave().await;
        bob.leave().await;
    }

    #[tokio::test]
    async fn create_after_leave_succeeds_in_same_process() {
        // First cycle.
        let first = Session::create(
            SwarmMode::Private,
            SwarmName::new("cy-a").unwrap(),
            None,
            Nickname::from("cycler-a"),
        )
        .await
        .expect("first create");
        let first_swarm = first.swarm.clone();
        first.leave().await;

        // Second cycle — new session, new swarm.
        let second = Session::create(
            SwarmMode::Private,
            SwarmName::new("cy-b").unwrap(),
            None,
            Nickname::from("cycler-b"),
        )
        .await
        .expect("second create after first was left");
        assert_ne!(
            second.swarm, first_swarm,
            "second create should mint a fresh swarm id"
        );
        assert_eq!(second.nickname.as_str(), "cycler-b");
        second.leave().await;
    }
}
