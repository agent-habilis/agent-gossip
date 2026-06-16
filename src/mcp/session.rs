//! The MCP server's swarm handle — a thin wrapper over the in-process
//! [`crate::embed::SwarmSession`] that adds the implicit `fetch` cursor. The rmcp
//! server, tool handlers, and arg/result types live in [`super`]; only the
//! session plumbing lives here, reused from `embed` rather than duplicated.

use std::sync::Mutex;

use anyhow::Result;

use crate::daemon::state::RosterSnapshot;
use crate::embed::{CreateConfig, CreateError, InProcessSession, JoinConfig, JoinError};
use crate::protocol::swarm::SwarmName;
use crate::protocol::{
    Message, MessageBody, MessageId, Nickname, SwarmId, TaskId, TaskKind, TaskPhase,
};

/// One active swarm for the MCP server: the shared [`InProcessSession`]
/// core (poll-only, silent) plus the per-session implicit `after` cursor.
/// Dropping it winds the loop down via the core's own `Drop`.
pub(super) struct Session {
    inner: InProcessSession,
    /// Implicit `after` cursor for [`Session::fetch_messages`].
    last_delivered_id: Mutex<Option<MessageId>>,
}

impl Session {
    /// Start a new swarm — poll-only, silent — from an embed [`CreateConfig`].
    ///
    /// # Errors
    /// Propagates [`CreateError`] so the tool layer can classify
    /// advertise-on-loopback (`invalid_params`) vs setup (`internal`).
    pub(super) async fn create(cfg: CreateConfig) -> Result<Self, CreateError> {
        Ok(Self::wrap(InProcessSession::create_poll(cfg).await?))
    }

    /// Join an existing swarm — poll-only, silent — from a [`JoinConfig`]
    /// (resolves the `ahs…`/domain/git-URL target internally).
    ///
    /// # Errors
    /// [`JoinError`] if the target can't be resolved or setup fails.
    pub(super) async fn join(cfg: JoinConfig) -> Result<Self, JoinError> {
        Ok(Self::wrap(InProcessSession::join_poll(cfg).await?))
    }

    fn wrap(inner: InProcessSession) -> Self {
        Self {
            inner,
            last_delivered_id: Mutex::new(None),
        }
    }

    /// The resolved swarm id.
    pub(super) fn swarm(&self) -> &SwarmId {
        self.inner.swarm_id()
    }

    /// The decoded swarm name.
    pub(super) fn name(&self) -> &SwarmName {
        self.inner.name()
    }

    /// Our effective nickname.
    pub(super) fn nickname(&self) -> &Nickname {
        self.inner.nickname()
    }

    /// Broadcast a message. Returns `Some((id, echo))` — the new id and the
    /// canonical [`Message`] — or `None` when the sender-side rate limiter
    /// dropped it. Advances the implicit cursor past our own send.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub(super) async fn send_message(
        &self,
        body: MessageBody,
        reply: Option<Nickname>,
    ) -> Result<Option<(MessageId, Message)>> {
        match self.inner.send(body, reply).await? {
            Some(msg) => {
                let id = msg.id.clone();
                self.advance_cursor_to(id.clone());
                Ok(Some((id, msg)))
            }
            None => Ok(None),
        }
    }

    /// Send one leg of a task exchange. Returns `Some((id, echo))` or
    /// `None` when the sender-side rate limiter dropped it. Advances the
    /// implicit cursor past our own send, like [`send_message`](Self::send_message).
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub(super) async fn send_task(
        &self,
        to: Nickname,
        task_id: TaskId,
        kind: TaskKind,
        phase: TaskPhase,
        body: MessageBody,
    ) -> Result<Option<(MessageId, Message)>> {
        match self.inner.task(to, task_id, kind, phase, body).await? {
            Some(msg) => {
                let id = msg.id.clone();
                self.advance_cursor_to(id.clone());
                Ok(Some((id, msg)))
            }
            None => Ok(None),
        }
    }

    /// Snapshot the live participant roster (active + quiet, recency-sorted).
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub(super) async fn peers(&self) -> Result<RosterSnapshot> {
        self.inner.peers().await
    }

    /// Fetch buffered messages after `after` (or the implicit cursor when
    /// `None`). Auto-advances the cursor and returns the advanced id.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub(super) async fn fetch_messages(
        &self,
        after: Option<MessageId>,
    ) -> Result<(Vec<Message>, Option<MessageId>)> {
        let msgs = self.inner.fetch(self.effective_after(after)).await?;
        let current_id = msgs.last().map(|msg| msg.id.clone());
        if let Some(id) = current_id.clone() {
            self.advance_cursor_to(id);
        }
        Ok((msgs, current_id))
    }

    /// Explicit cursor wins; otherwise fall back to the implicit one.
    fn effective_after(&self, explicit: Option<MessageId>) -> Option<MessageId> {
        explicit.or_else(|| self.last_delivered_id.lock().unwrap().clone())
    }

    fn advance_cursor_to(&self, id: MessageId) {
        *self.last_delivered_id.lock().unwrap() = Some(id);
    }

    /// Clean shutdown — delegates to the core's `leave`.
    pub(super) async fn leave(self) {
        let _ = self.inner.leave().await;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Message, MessageBody, MessageId, Nickname, Session, SwarmId, SwarmName};
    use crate::embed::{CreateConfig, JoinConfig};
    use crate::protocol::{MessageKind, PresenceSubtype};
    use crate::resolver::JoinTarget;

    // All tests use the private network (loopback) so they work on
    // any CI without public iroh DNS / relay access.

    /// A loopback create config with an explicit nickname, no advertising.
    fn create_cfg(name: &str, nick: &str) -> CreateConfig {
        let mut cfg = CreateConfig::new(SwarmName::new(name).unwrap());
        cfg.nickname = Some(Nickname::from(nick));
        cfg
    }

    /// A join config for an existing swarm id with an explicit nickname.
    fn join_cfg(swarm: &SwarmId, nick: &str) -> JoinConfig {
        let mut cfg = JoinConfig::new(JoinTarget::Swarm(swarm.clone()));
        cfg.nickname = Some(Nickname::from(nick));
        cfg
    }

    async fn wait_for_gossip(session: &Session, author: &str, body: &str) -> Option<MessageId> {
        // Poll up to ~10 s for the message to propagate via gossip.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            if let Ok((msgs, _)) = session.fetch_messages(None).await {
                for entry in &msgs {
                    if entry.author.as_str() == author && entry.body.as_str() == body {
                        return Some(entry.id.clone());
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        None
    }

    #[tokio::test]
    async fn create_session_yields_valid_swarm_and_nickname() {
        let session = Session::create(create_cfg("test1", "alice-test"))
            .await
            .expect("create");
        assert!(session.swarm().as_str().starts_with("ahs"));
        assert_eq!(session.name().as_str(), "test1");
        assert_eq!(session.nickname().as_str(), "alice-test");
        session.leave().await;
    }

    #[tokio::test]
    async fn two_sessions_same_swarm_exchange_messages() {
        let creator = Session::create(create_cfg("two", "alice-two"))
            .await
            .expect("create");
        let swarm = creator.swarm().clone();

        let joiner = Session::join(join_cfg(&swarm, "bob-two"))
            .await
            .expect("join");
        assert_eq!(joiner.name().as_str(), "two");

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
        let alice = Session::create(create_cfg("replay", "alice-replay"))
            .await
            .expect("create");

        let (sent, echo) = alice
            .send_message(MessageBody::from("self-echo"), None)
            .await
            .expect("send_message")
            .expect("within rate limit");
        assert_eq!(echo.id, sent);
        assert_eq!(echo.author.as_str(), "alice-replay");
        assert_eq!(echo.body.as_str(), "self-echo");
        assert!(echo.timestamp > 0, "echo must carry a unix timestamp");

        // Cursor advanced past self-send → default fetch hides it.
        let (msgs, _) = alice.fetch_messages(None).await.expect("fetch");
        let has_own = msgs.iter().any(|msg| msg.id == sent);
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
        let alice = Session::create(create_cfg("cursor", "alice-cursor"))
            .await
            .expect("create");
        let swarm = alice.swarm().clone();
        let bob = Session::join(join_cfg(&swarm, "bob-cursor"))
            .await
            .expect("join");

        // Drive alice's first cursor-less fetch until bob's join
        // presence lands. That first non-empty fetch advances the
        // implicit cursor past everything currently buffered —
        // which is exactly what we want to test against next.
        let mut first: Vec<Message> = Vec::new();
        let mut expected_cursor: Option<MessageId> = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            let (msgs, cur) = alice.fetch_messages(None).await.expect("first fetch");
            if msgs.iter().any(|msg| {
                matches!(
                    msg.kind,
                    MessageKind::Presence {
                        subtype: PresenceSubtype::Joined
                    }
                ) && msg.author.as_str() == "bob-cursor"
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
        let mut saw: Vec<Message> = Vec::new();
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
        assert_eq!(saw[0].body.as_str(), "hi via cursor");

        // Explicit `after` must override the implicit cursor: pass
        // the cursor position we observed earlier and confirm we
        // still get bob's message back.
        let (forced, _) = alice
            .fetch_messages(Some(expected_cursor))
            .await
            .expect("explicit fetch");
        assert!(
            forced
                .iter()
                .any(|msg| msg.body.as_str() == "hi via cursor"),
            "explicit after must override implicit cursor"
        );

        alice.leave().await;
        bob.leave().await;
    }

    #[tokio::test]
    async fn create_after_leave_succeeds_in_same_process() {
        // First cycle.
        let first = Session::create(create_cfg("cy-a", "cycler-a"))
            .await
            .expect("first create");
        let first_swarm = first.swarm().clone();
        first.leave().await;

        // Second cycle — new session, new swarm.
        let second = Session::create(create_cfg("cy-b", "cycler-b"))
            .await
            .expect("second create after first was left");
        assert_ne!(
            second.swarm(),
            &first_swarm,
            "second create should mint a fresh swarm id"
        );
        assert_eq!(second.nickname().as_str(), "cycler-b");
        second.leave().await;
    }
}
