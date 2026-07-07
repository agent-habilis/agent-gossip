//! The MCP server's mesh handle — a thin wrapper over the in-process
//! [`crate::embed::MeshSession`] that adds the implicit `fetch` cursor. The rmcp
//! server, tool handlers, and arg/result types live in [`super`]; only the
//! session plumbing lives here, reused from `embed` rather than duplicated.

use std::sync::Mutex;

use anyhow::Result;

use crate::a2a::TaskId;
use crate::embed::{
    A2aCallParams, CreateConfig, CreateError, InProcessSession, JoinConfig, JoinError,
    TaskArtifactParams, TopicConfig,
};
use agent_habilis_mesh::daemon::state::RosterSnapshot;
use agent_habilis_mesh::protocol::mesh::MeshName;
use agent_habilis_mesh::protocol::{MeshId, Message, MessageBody, MessageId, Nickname};

/// One active mesh for the MCP server: the shared [`InProcessSession`]
/// core (poll-only, silent) plus the per-session implicit `after` cursor.
/// Dropping it winds the loop down via the core's own `Drop`.
pub(super) struct Session {
    inner: InProcessSession,
    /// Implicit `after` seq cursor for [`Session::fetch_messages`].
    last_delivered_seq: Mutex<Option<u64>>,
}

impl Session {
    /// Start a new mesh — poll-only, silent — from an embed [`CreateConfig`].
    ///
    /// # Errors
    /// Propagates [`CreateError`] so the tool layer can classify
    /// advertise-on-loopback (`invalid_params`) vs setup (`internal`).
    pub(super) async fn create(cfg: CreateConfig) -> Result<Self, CreateError> {
        Ok(Self::wrap(InProcessSession::create_poll(cfg).await?))
    }

    /// Join an existing mesh — poll-only, silent — from a [`JoinConfig`]
    /// (decodes the `💬…` id target internally).
    ///
    /// # Errors
    /// [`JoinError`] if the target can't be resolved or setup fails.
    pub(super) async fn join(cfg: JoinConfig) -> Result<Self, JoinError> {
        Ok(Self::wrap(InProcessSession::join_poll(cfg).await?))
    }

    /// Join a topic — poll-only, silent — from a [`TopicConfig`]: a public
    /// mesh derived deterministically from a shared string.
    ///
    /// # Errors
    /// [`JoinError`] on setup failure.
    pub(super) async fn topic(cfg: TopicConfig) -> Result<Self, JoinError> {
        Ok(Self::wrap(InProcessSession::topic_poll(cfg).await?))
    }

    fn wrap(inner: InProcessSession) -> Self {
        Self {
            inner,
            last_delivered_seq: Mutex::new(None),
        }
    }

    /// The resolved mesh id.
    pub(super) fn mesh(&self) -> &MeshId {
        self.inner.mesh_id()
    }

    /// The decoded mesh name.
    pub(super) fn name(&self) -> &MeshName {
        self.inner.name()
    }

    /// Our effective nickname.
    pub(super) fn nickname(&self) -> &Nickname {
        self.inner.nickname()
    }

    /// Broadcast a message. Returns `(id, echo)` — the new id and the
    /// canonical [`Message`]. The full echo is returned here, so a caller
    /// need not re-fetch to see its own send; the self-echo also surfaces
    /// once in a later `fetch_messages` (with `self:true`), matching the
    /// live stream.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub(super) async fn send_message(&self, body: MessageBody) -> Result<(MessageId, Message)> {
        let msg = self.inner.send(body).await?;
        Ok((msg.id.clone(), msg))
    }

    /// Worker-emit a task `TaskStatusUpdate`. Returns `(id, echo)`.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub(super) async fn task_status(
        &self,
        task_id: TaskId,
        state: crate::a2a::TaskState,
        note: Option<String>,
    ) -> Result<(MessageId, Message)> {
        let msg = self.inner.task_status(task_id, state, note).await?;
        Ok((msg.id.clone(), msg))
    }

    /// Worker-emit a task `TaskArtifactUpdate` (the result). Returns `(id, echo)`.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub(super) async fn task_artifact(
        &self,
        artifact: TaskArtifactParams,
    ) -> Result<(MessageId, Message)> {
        let TaskArtifactParams {
            task_id,
            text,
            file,
            file_name,
            file_mime,
        } = artifact;
        let file = file.map(|path| agent_habilis_mesh::blob::FileRef {
            path,
            name: file_name,
            mime: file_mime,
        });
        let msg = self.inner.task_artifact(task_id, text, file).await?;
        Ok((msg.id.clone(), msg))
    }

    /// Call a peer's A2A server over gossip (request/response); returns the
    /// parsed JSON-RPC response.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub(super) async fn a2a_call(&self, call: A2aCallParams) -> Result<serde_json::Value> {
        self.inner.a2a_call(call).await
    }

    /// Snapshot the live participant roster (active + quiet, recency-sorted).
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub(super) async fn peers(&self) -> Result<RosterSnapshot> {
        self.inner.peers().await
    }

    /// Run an RTT round and return the per-peer rows. Blocks for the ping
    /// window (a few seconds) while pongs are collected.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub(super) async fn ping(&self) -> Result<Vec<crate::output::PingPeer>> {
        self.inner.ping().await
    }

    /// Apply an RFC 7386 JSON Merge Patch to the shared state. Any JSON value is a
    /// valid merge.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub(super) async fn apply_state_merge(&self, merge: serde_json::Value) -> Result<()> {
        self.inner.state_merge(merge).await
    }

    /// The current derived shared-state document (the merge fold).
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub(super) async fn state_get(&self) -> Result<serde_json::Value> {
        self.inner.state_get().await
    }

    /// `meta`-channel counterpart of [`apply_state_merge`](Self::apply_state_merge).
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub(super) async fn apply_meta_merge(&self, merge: serde_json::Value) -> Result<()> {
        self.inner.meta_merge(merge).await
    }

    /// `meta`-channel counterpart of [`state_get`](Self::state_get).
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub(super) async fn meta_get(&self) -> Result<serde_json::Value> {
        self.inner.meta_get().await
    }

    /// Fetch surfaced events after `after`, or after the implicit seq cursor
    /// when `after` is `None`.
    ///
    /// The implicit cursor advances **only on the cursor-less call** (`after =
    /// None`): that is the idle-loop path, where each fetch should return only
    /// new traffic. An **explicit `after` is a non-mutating replay** — it reads
    /// from the given point without disturbing the session's implicit cursor
    /// (so a one-off `after: 0` to inspect history can't desync the loop).
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub(super) async fn fetch_messages(
        &self,
        after: Option<u64>,
        long: bool,
    ) -> Result<Vec<crate::a2a::surfaced::SurfacedEvent>> {
        let events = self.inner.fetch(self.effective_after(after), long).await?;
        if after.is_none()
            && let Some(seq) = events.last().map(|item| item.seq)
        {
            self.advance_cursor_to(seq);
        }
        Ok(events)
    }

    /// Explicit cursor wins; otherwise fall back to the implicit one.
    fn effective_after(&self, explicit: Option<u64>) -> Option<u64> {
        explicit.or_else(|| *self.last_delivered_seq.lock().unwrap())
    }

    fn advance_cursor_to(&self, seq: u64) {
        *self.last_delivered_seq.lock().unwrap() = Some(seq);
    }

    /// Clean shutdown — delegates to the core's `leave`.
    pub(super) async fn leave(self) {
        let _ = self.inner.leave().await;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::{MeshId, MeshName, Message, MessageBody, MessageId, Nickname, Session};
    use crate::embed::{CreateConfig, JoinConfig};
    use agent_habilis_mesh::protocol::{MessageKind, PresenceSubtype};
    use agent_habilis_mesh::resolver::JoinTarget;

    // All tests use the private network (loopback) so they work on
    // any CI without public iroh DNS / relay access.

    /// A loopback create config with an explicit nickname, no advertising.
    fn create_cfg(name: &str, nick: &str) -> CreateConfig {
        let mut cfg = CreateConfig::new(MeshName::new(name).unwrap());
        cfg.nickname = Some(Nickname::from(nick));
        cfg
    }

    /// A join config for an existing mesh id with an explicit nickname.
    fn join_cfg(mesh: &MeshId, nick: &str) -> JoinConfig {
        let mut cfg = JoinConfig::new(JoinTarget::Mesh(mesh.clone()));
        cfg.nickname = Some(Nickname::from(nick));
        cfg
    }

    /// The `Message` inside a surfaced `msg`/`presence`/`task` event, if
    /// any — the test view into the now-structured `fetch_messages` result.
    fn as_message(event: &crate::output::OutputEvent) -> Option<&Message> {
        use crate::output::OutputEvent;
        match event {
            OutputEvent::Message { msg, .. }
            | OutputEvent::Presence { msg }
            | OutputEvent::Task { msg, .. } => Some(msg),
            OutputEvent::Ready { .. }
            | OutputEvent::MeshId { .. }
            | OutputEvent::PeerTimeout { .. }
            | OutputEvent::PeerReturn { .. }
            | OutputEvent::Fork { .. }
            | OutputEvent::MsgPosted { .. }
            | OutputEvent::Info { .. }
            | OutputEvent::Error { .. }
            | OutputEvent::PingReport { .. }
            | OutputEvent::StateChanged { .. }
            | OutputEvent::TaskMessage { .. }
            | OutputEvent::TaskTimeout { .. } => None,
        }
    }

    /// Generous in-process delivery budget. Every mesh wait below is adaptive —
    /// it breaks the instant the condition holds — so a healthy run returns in
    /// milliseconds and only a genuinely stalled link pays this ceiling. Set
    /// high so a loaded host (concurrent tests, busy CI) can't flake a correct
    /// delivery; mirrors the integration suite's `MSG_TIMEOUT`.
    const DELIVER: Duration = Duration::from_mins(1);

    async fn wait_for_gossip(session: &Session, author: &str, body: &str) -> Option<MessageId> {
        // Poll up to `DELIVER` for the message to propagate via gossip.
        let deadline = tokio::time::Instant::now() + DELIVER;
        while tokio::time::Instant::now() < deadline {
            if let Ok(events) = session.fetch_messages(None, false).await {
                for entry in &events {
                    if let Some(msg) = as_message(&entry.event)
                        && msg.author.as_str() == author
                        && crate::a2a::gossip::chat_text(msg).as_deref() == Some(body)
                    {
                        return Some(msg.id.clone());
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        None
    }

    #[tokio::test]
    async fn create_session_yields_valid_mesh_and_nickname() {
        let session = Session::create(create_cfg("test1", "alice-test"))
            .await
            .expect("create");
        assert!(session.mesh().as_str().starts_with("💬"));
        assert_eq!(session.name().as_str(), "test1");
        assert_eq!(session.nickname().as_str(), "alice-test");
        session.leave().await;
    }

    #[tokio::test]
    async fn two_sessions_same_mesh_task_messages() {
        let creator = Session::create(create_cfg("two", "alice-two"))
            .await
            .expect("create");
        let mesh = creator.mesh().clone();

        let joiner = Session::join(join_cfg(&mesh, "bob-two"))
            .await
            .expect("join");
        assert_eq!(joiner.name().as_str(), "two");

        // Send from creator → joiner should see it.
        let (sent_id, _) = creator
            .send_message(MessageBody::from("hi bob"))
            .await
            .expect("send_message");

        let observed = wait_for_gossip(&joiner, "alice-two", "hi bob").await;
        assert_eq!(
            observed,
            Some(sent_id),
            "joiner should receive message body=hi bob from alice-two with matching id"
        );

        // And the reverse direction.
        let (reply_id, _) = joiner
            .send_message(MessageBody::from("hi alice"))
            .await
            .expect("send_message reply");
        let observed2 = wait_for_gossip(&creator, "bob-two", "hi alice").await;
        assert_eq!(observed2, Some(reply_id));

        joiner.leave().await;
        creator.leave().await;
    }

    #[tokio::test]
    async fn long_poll_blocks_then_returns_on_peer_traffic() {
        let creator = Session::create(create_cfg("lp", "alice-lp"))
            .await
            .expect("create");
        let mesh = creator.mesh().clone();
        let joiner = Session::join(join_cfg(&mesh, "bob-lp"))
            .await
            .expect("join");

        // Mesh first (a delivered message proves the link) so the long-poll
        // below is waiting on a *fresh* event, not racing initial bootstrap.
        creator
            .send_message(MessageBody::from("warmup"))
            .await
            .expect("send warmup");
        assert!(
            wait_for_gossip(&joiner, "alice-lp", "warmup")
                .await
                .is_some(),
            "mesh established"
        );

        // Baseline the joiner's cursor past the warmup, then block on a fetch
        // with a generous wait while the creator sends after a short delay.
        // `tokio::join!` runs both concurrently without `'static` (so neither
        // session needs cloning).
        let after = joiner
            .fetch_messages(None, false)
            .await
            .expect("baseline")
            .last()
            .map(|item| item.seq);

        let started = tokio::time::Instant::now();
        let delayed_send = async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            creator
                .send_message(MessageBody::from("after warmup"))
                .await
                .expect("send");
        };
        // The blocking fetch should resolve as soon as the gossiped "after
        // warmup" lands — well under the park cap — not spin to the timeout. A
        // single long-poll may return before gossip propagation completes, so
        // re-issue until the message shows (each call still parks). The park is
        // the daemon's 60s long-poll cap, so under a loaded host a correct
        // delivery still arrives long before the call would time out empty.
        let watch = async {
            loop {
                let events = joiner
                    .fetch_messages(after, true)
                    .await
                    .expect("long-poll fetch");
                if events
                    .iter()
                    .filter_map(|item| as_message(&item.event))
                    .any(|msg| {
                        crate::a2a::gossip::chat_text(msg).as_deref() == Some("after warmup")
                    })
                {
                    break;
                }
            }
        };
        let watch = tokio::time::timeout(Duration::from_secs(90), watch);
        let ((), watched) = tokio::join!(delayed_send, watch);
        assert!(
            watched.is_ok(),
            "long-poll delivered the post-warmup message"
        );
        // Returning before the 60s wait ceiling proves the poll woke on the
        // traffic rather than spinning to an empty timeout. Generous margin
        // (real delivery is sub-second to seconds) so load can't flake it.
        assert!(
            started.elapsed() < DELIVER,
            "long-poll woke on traffic rather than spinning to its wait ceiling"
        );

        joiner.leave().await;
        creator.leave().await;
    }

    // The empty-timeout shape (park elapses quietly → `[]`) can't be tested
    // here without waiting the full 60s cap: the `Tuning` OnceLock is
    // process-wide, so an in-process test can't shrink it per-test. It is
    // covered by the state-level deadline test
    // (`poll_or_register_long_parks_then_expires_at_cap`) and the MCP-stdio
    // subprocess test, which shortens the cap via `--longpoll-max-ms`.

    #[tokio::test]
    async fn send_message_returns_full_echo_and_surfaces_self() {
        // send_message returns an authoritative echo (id, author, ts, body)
        // so callers don't need to re-fetch to see their own send. With
        // stream-parity, the self-echo also surfaces once in a later fetch
        // tagged `self:true` — the same as the live `--output json` stream.
        let alice = Session::create(create_cfg("replay", "alice-replay"))
            .await
            .expect("create");

        let (sent, echo) = alice
            .send_message(MessageBody::from("self-echo"))
            .await
            .expect("send_message");
        assert_eq!(echo.id, sent);
        assert_eq!(echo.author.as_str(), "alice-replay");
        assert_eq!(
            crate::a2a::gossip::chat_text(&echo).as_deref(),
            Some("self-echo")
        );
        assert!(echo.timestamp > 0, "echo must carry a unix timestamp");

        // The self-send surfaces in a fetch, marked `self:true`.
        let mut saw_self = false;
        let deadline = tokio::time::Instant::now() + DELIVER;
        while tokio::time::Instant::now() < deadline && !saw_self {
            let events = alice.fetch_messages(None, false).await.expect("fetch");
            saw_self = events.iter().any(|item| {
                matches!(
                    &item.event,
                    crate::output::OutputEvent::Message { msg, is_self }
                        if *is_self && msg.id == sent
                )
            });
            if !saw_self {
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        }
        assert!(saw_self, "own send should surface once with self:true");

        alice.leave().await;
    }

    #[tokio::test]
    async fn implicit_cursor_returns_delta_on_subsequent_fetches() {
        // First `fetch_messages(None)` sees full history. Subsequent
        // `fetch_messages(None)` calls see only what arrived since the last one
        // (the cursor-less call advances the implicit cursor). An explicit
        // `after` is a non-mutating replay that does NOT touch the cursor.
        let alice = Session::create(create_cfg("cursor", "alice-cursor"))
            .await
            .expect("create");
        let mesh = alice.mesh().clone();
        let bob = Session::join(join_cfg(&mesh, "bob-cursor"))
            .await
            .expect("join");

        // Drive alice's first cursor-less fetch until bob's join presence lands.
        // That first non-empty fetch advances the implicit cursor past
        // everything currently buffered — which is exactly what we test next.
        // Capture the seq just BEFORE bob's join (the prior event's seq) so a
        // later explicit replay from there re-reads bob's join + send.
        let mut replay_from: Option<u64> = None;
        let deadline = tokio::time::Instant::now() + DELIVER;
        while tokio::time::Instant::now() < deadline {
            let events = alice
                .fetch_messages(None, false)
                .await
                .expect("first fetch");
            let bob_join_idx = events.iter().position(|item| {
                as_message(&item.event).is_some_and(|msg| {
                    matches!(
                        msg.kind,
                        MessageKind::Presence {
                            subtype: PresenceSubtype::Joined
                        }
                    ) && msg.author.as_str() == "bob-cursor"
                })
            });
            if let Some(idx) = bob_join_idx {
                // The seq strictly before bob's join (0 if it's the first event)
                // — replaying from here re-includes the join and everything after.
                replay_from = Some(idx.checked_sub(1).map_or(0, |prev| events[prev].seq));
                break;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        let replay_from = replay_from.expect("first fetch should see bob's join presence");

        // Second fetch with no new traffic: cursor advanced past everything
        // buffered, so the delta must be empty.
        let empty_delta = alice
            .fetch_messages(None, false)
            .await
            .expect("delta fetch");
        assert!(
            empty_delta.is_empty(),
            "second cursor-less fetch must return delta (empty), got {empty_delta:?}"
        );

        // Bob sends — alice's next cursor-less fetch must surface bob's
        // message, nothing older.
        bob.send_message(MessageBody::from("hi via cursor"))
            .await
            .expect("send");
        let mut saw_body = false;
        let delta_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < delta_deadline && !saw_body {
            let events = alice
                .fetch_messages(None, false)
                .await
                .expect("delta fetch 2");
            saw_body = events
                .iter()
                .filter_map(|item| as_message(&item.event))
                .any(|msg| crate::a2a::gossip::chat_text(msg).as_deref() == Some("hi via cursor"));
            if !saw_body {
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        }
        assert!(
            saw_body,
            "delta fetch after bob's send should surface body=hi via cursor"
        );

        // Explicit `after` is a non-mutating replay from an earlier seq: it must
        // re-surface bob's message regardless of where the implicit cursor sits.
        let forced = alice
            .fetch_messages(Some(replay_from), false)
            .await
            .expect("explicit fetch");
        assert!(
            forced
                .iter()
                .filter_map(|item| as_message(&item.event))
                .any(|msg| crate::a2a::gossip::chat_text(msg).as_deref() == Some("hi via cursor")),
            "explicit after must replay from the given seq"
        );

        // And the explicit replay must NOT have disturbed the implicit cursor:
        // a following cursor-less fetch sees no new traffic (empty).
        let after_replay = alice
            .fetch_messages(None, false)
            .await
            .expect("post-replay fetch");
        assert!(
            after_replay.is_empty(),
            "explicit replay must not advance the implicit cursor, got {after_replay:?}"
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
        let first_mesh = first.mesh().clone();
        first.leave().await;

        // Second cycle — new session, new mesh.
        let second = Session::create(create_cfg("cy-b", "cycler-b"))
            .await
            .expect("second create after first was left");
        assert_ne!(
            second.mesh(),
            &first_mesh,
            "second create should mint a fresh mesh id"
        );
        assert_eq!(second.nickname().as_str(), "cycler-b");
        second.leave().await;
    }

    /// An object merge applies and is reflected by `state_get`; a non-object
    /// top-level merge is rejected (automerge's document root is always a map, so
    /// the old RFC 7386 "replace the whole document" case has no representation).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn apply_state_merge_applies_and_reads_back() {
        let session = Session::create(create_cfg("merge", "alice"))
            .await
            .expect("create");

        session
            .apply_state_merge(json!({"turn": "a"}))
            .await
            .expect("an object merge applies");
        assert_eq!(
            session.state_get().await.expect("state_get")["turn"],
            json!("a")
        );

        // A non-object top-level merge cannot be represented on the automerge
        // root and is refused — the prior document stands.
        assert!(
            session.apply_state_merge(json!([1, 2, 3])).await.is_err(),
            "a non-object top-level merge must be rejected"
        );
        assert_eq!(
            session.state_get().await.expect("state_get"),
            json!({"turn": "a"})
        );

        session.leave().await;
    }
}
