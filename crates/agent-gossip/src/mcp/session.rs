//! The MCP server's mesh handle — a thin wrapper over the in-process
//! [`crate::api::InProcessSession`] that adds the implicit `fetch` cursor. The rmcp
//! server, tool handlers, and arg/result types live in [`super`]; only the
//! session plumbing lives here, reused from `api` rather than duplicated.

use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use habilis_network::protocol::AuthorKey;

use crate::a2a::surfaced_event::PollBatch;
use crate::api::{
    CreateConfig, CreateError, InProcessSession, JoinConfig, JoinError, SessionError, TopicConfig,
};

/// One active mesh for the MCP server: the shared [`InProcessSession`]
/// core (poll-only, silent) plus the per-session implicit `after` cursor.
/// Dropping it winds the loop down via the core's own `Drop`.
pub(crate) struct Session {
    pub(super) core: InProcessSession,
    own_key: AuthorKey,
    /// Implicit `after` seq cursor for [`Session::fetch_messages`]. It starts at 0, the
    /// before-anything cursor.
    last_delivered_seq: AtomicU64,
}

/// Our author key, or the setup failure that a stopped event loop leaves behind. A session
/// without its key cannot address its meta entry, so there is no session without it.
fn require_own_key(own_key: Result<AuthorKey, SessionError>) -> Result<AuthorKey> {
    own_key.map_err(|error| anyhow::anyhow!("the mesh session has no author key: {error}"))
}

impl Session {
    /// Start a new mesh — poll-only, silent — from an api [`CreateConfig`].
    ///
    /// # Errors
    /// Propagates [`CreateError`] so the tool layer can classify
    /// advertise-on-loopback (`invalid_params`) vs setup (`internal`).
    pub(super) async fn create(config: CreateConfig) -> Result<Self, CreateError> {
        let core = InProcessSession::create_poll(config).await?;
        let own_key = require_own_key(core.own_key().await).map_err(CreateError::Setup)?;
        Ok(Self::wrap(core, own_key))
    }

    /// Join an existing mesh — poll-only, silent — from a [`JoinConfig`]
    /// (decodes the id target internally).
    ///
    /// # Errors
    /// [`JoinError`] if the target can't be resolved or setup fails.
    pub(super) async fn join(config: JoinConfig) -> Result<Self, JoinError> {
        let core = InProcessSession::join_poll(config).await?;
        let own_key = require_own_key(core.own_key().await).map_err(JoinError::Setup)?;
        Ok(Self::wrap(core, own_key))
    }

    /// Join a topic — poll-only, silent — from a [`TopicConfig`]: a public
    /// mesh derived deterministically from a shared string.
    ///
    /// # Errors
    /// [`JoinError`] on setup failure.
    pub(super) async fn topic(config: TopicConfig) -> Result<Self, JoinError> {
        let core = InProcessSession::topic_poll(config).await?;
        let own_key = require_own_key(core.own_key().await).map_err(JoinError::Setup)?;
        Ok(Self::wrap(core, own_key))
    }

    fn wrap(core: InProcessSession, own_key: AuthorKey) -> Self {
        Self {
            core,
            own_key,
            last_delivered_seq: AtomicU64::new(0),
        }
    }

    /// Our author key: the slot of our meta entry is its hex.
    pub(super) fn own_key(&self) -> AuthorKey {
        self.own_key
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
    pub(super) async fn fetch_messages(&self, after: Option<u64>, long: bool) -> Result<PollBatch> {
        // Never `None`: MCP keeps its own per-session cursor, so it always speaks the
        // explicit-seq protocol. The daemon-side read cursor and its waking-only bell
        // semantics belong to the cursor-less CLI `poll` alone.
        let cursor = after.unwrap_or_else(|| self.last_delivered_seq.load(Ordering::Relaxed));
        let batch = self.core.fetch(Some(cursor), long).await?;
        if after.is_none()
            && let Some(seq) = batch.events.last().map(|item| item.seq)
        {
            self.last_delivered_seq.store(seq, Ordering::Relaxed);
        }
        Ok(batch)
    }

    /// Clean shutdown — delegates to the core's `leave`.
    pub(super) async fn leave(self) {
        let _ = self.core.leave().await;
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::time::Duration;

    use habilis_network::protocol::{
        AuthorKey, JoinTarget, MeshId, MeshName, Message, MessageBody, MessageId, MessageKind,
        Nickname, PresenceSubtype,
    };
    use serde_json::json;

    use super::Session;
    use crate::a2a::status_update_params::chat_text;
    use crate::a2a::surfaced_event::SurfacedEvent;
    use crate::api::{CreateConfig, JoinConfig};

    // All tests use the private network (loopback) so they work on
    // any CI without public iroh DNS / relay access.

    /// A loopback create config with an explicit nickname, no advertising.
    fn create_cfg(name: &str, nick: &str) -> CreateConfig {
        CreateConfig::new(MeshName::try_new(name).expect("the test literal is a valid mesh name"))
            .with_nickname(Nickname::from(nick))
    }

    /// A join config for an existing mesh id with an explicit nickname.
    fn join_cfg(mesh: &MeshId, nick: &str) -> JoinConfig {
        JoinConfig::new(JoinTarget::Mesh(mesh.clone())).with_nickname(Nickname::from(nick))
    }

    /// The `Message` inside a surfaced `msg`/`presence`/`task` event, if
    /// any — the test view into the now-structured `fetch_messages` result.
    fn as_message(event: &crate::emission::OutputEvent) -> Option<&Message> {
        use crate::emission::OutputEvent;
        match event {
            OutputEvent::Message { message, .. }
            | OutputEvent::Presence { message, .. }
            | OutputEvent::Task { message, .. } => Some(message),
            OutputEvent::Ready { .. }
            | OutputEvent::PeerTimeout { .. }
            | OutputEvent::PeerReturn { .. }
            | OutputEvent::Info { .. }
            | OutputEvent::Error { .. }
            | OutputEvent::NicknameHeld { .. }
            | OutputEvent::EventsLost { .. }
            | OutputEvent::PingReport { .. }
            | OutputEvent::StateChanged { .. }
            | OutputEvent::TaskMessage { .. }
            | OutputEvent::TaskTimeout { .. }
            | OutputEvent::TaskOverdue { .. }
            | OutputEvent::CheckinDue { .. }
            | OutputEvent::TaskGap { .. }
            | OutputEvent::Receipt { .. } => None,
        }
    }

    /// Whether `event` is the self-echo of `sent` — a `Message` event
    /// tagged `is_self:true` whose id matches.
    fn is_self_echo(event: &crate::emission::OutputEvent, sent: &MessageId) -> bool {
        matches!(
            event,
            crate::emission::OutputEvent::Message { message, is_self, .. }
                if *is_self && message.id == *sent
        )
    }

    /// The seq strictly before bob-cursor's join in `events` (0 if the join is
    /// the first event) — replaying from here re-includes the join and
    /// everything after.
    fn seq_before_bob_join(events: &[SurfacedEvent]) -> Option<u64> {
        let join_idx = events.iter().position(|item| is_bob_join(&item.event))?;
        Some(
            join_idx
                .checked_sub(1)
                .map_or(0, |previous| events[previous].seq),
        )
    }

    /// Whether `event` is bob-cursor's join presence — the marker
    /// `implicit_cursor_returns_delta_on_subsequent_fetches` waits on to know
    /// its cursor baseline includes the join.
    fn is_bob_join(event: &crate::emission::OutputEvent) -> bool {
        as_message(event).is_some_and(|message| {
            matches!(
                message.kind,
                MessageKind::Presence {
                    subtype: PresenceSubtype::Joined
                }
            ) && message.author.as_str() == "bob-cursor"
        })
    }

    /// Generous in-process delivery budget. Every mesh wait below is adaptive —
    /// it breaks the instant the condition holds — so a healthy run returns in
    /// milliseconds and only a genuinely stalled link pays this ceiling. Set
    /// high so a loaded host (concurrent tests, busy CI) can't flake a correct
    /// delivery; mirrors the integration suite's `MSG_TIMEOUT`.
    const DELIVER: Duration = Duration::from_mins(1);

    /// Retry `probe` on `interval` until it yields `Some` or `deadline` passes
    /// — the shared shape behind every "wait for gossip to land" poll below.
    async fn poll_until<Fut, T>(
        deadline: tokio::time::Instant,
        interval: Duration,
        mut probe: impl FnMut() -> Fut,
    ) -> Option<T>
    where
        Fut: Future<Output = Option<T>>,
    {
        while tokio::time::Instant::now() < deadline {
            if let Some(value) = probe().await {
                return Some(value);
            }
            tokio::time::sleep(interval).await;
        }
        None
    }

    async fn wait_for_gossip(session: &Session, author: &str, body: &str) -> Option<MessageId> {
        wait_for_gossip_until(session, author, body, tokio::time::Instant::now() + DELIVER).await
    }

    async fn wait_for_gossip_until(
        session: &Session,
        author: &str,
        body: &str,
        deadline: tokio::time::Instant,
    ) -> Option<MessageId> {
        poll_until(deadline, Duration::from_millis(200), || async {
            let events = session.fetch_messages(None, false).await.ok()?.events;
            events.iter().find_map(|entry| {
                let message = as_message(&entry.event)?;
                (message.author.as_str() == author && chat_text(message).as_deref() == Some(body))
                    .then(|| message.id.clone())
            })
        })
        .await
    }

    /// Bring up the link from `from` to `to`: send a warmup and resend it until `to`
    /// fetches it. One send can land before the gossip link exists, and nothing
    /// resends it, so a lone warmup can be lost. This is only a precondition: the
    /// assertions that follow are unchanged, and the resends are bounded by the
    /// same delivery budget as every other wait here.
    async fn establish_link(from: &Session, to: &Session, from_nick: &str) {
        let deadline = tokio::time::Instant::now() + DELIVER;
        loop {
            from.core
                .broadcast(MessageBody::from("warmup"))
                .await
                .expect("send warmup");
            let window = (tokio::time::Instant::now() + Duration::from_secs(2)).min(deadline);
            if wait_for_gossip_until(to, from_nick, "warmup", window)
                .await
                .is_some()
            {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "mesh established: warmup from {from_nick} never reached the other session"
            );
        }
    }

    /// Long-poll `session` (an `after`-anchored fetch, so it parks rather
    /// than busy-spins) until a message with the given body surfaces.
    async fn watch_for_body(session: &Session, after: Option<u64>, body: &str) {
        loop {
            let events = session
                .fetch_messages(after, true)
                .await
                .expect("long-poll fetch")
                .events;
            let found = events
                .iter()
                .filter_map(|item| as_message(&item.event))
                .any(|message| chat_text(message).as_deref() == Some(body));
            if found {
                break;
            }
        }
    }

    #[tokio::test]
    async fn create_session_yields_valid_mesh_and_nickname() {
        let session = Session::create(create_cfg("test1", "alice-test"))
            .await
            .expect("a loopback create needs no network and cannot fail");
        assert!(
            MeshId::try_new(session.core.mesh_id().as_str()).is_ok(),
            "the mesh name is a valid mesh id"
        );
        assert_eq!(
            session.core.name().as_str(),
            "test1",
            "the session reports the mesh name it joined"
        );
        assert_eq!(
            session.core.nickname().as_str(),
            "alice-test",
            "the session reports its nickname"
        );
        session.leave().await;
    }

    #[tokio::test]
    async fn two_sessions_same_mesh_task_messages() {
        let creator = Session::create(create_cfg("two", "alice-two"))
            .await
            .expect("a loopback create needs no network and cannot fail");
        let mesh = creator.core.mesh_id().clone();

        let joiner = Session::join(join_cfg(&mesh, "bob-two"))
            .await
            .expect("the creator is alive and its mesh id is valid, so a loopback join succeeds");
        assert_eq!(
            joiner.core.name().as_str(),
            "two",
            "the joiner reports the mesh name"
        );

        establish_link(&creator, &joiner, "alice-two").await;
        establish_link(&joiner, &creator, "bob-two").await;

        // Send from creator → joiner should see it.
        let sent_id = creator
            .core
            .broadcast(MessageBody::from("hi bob"))
            .await
            .expect("a live session accepts a broadcast while the mesh is up")
            .id
            .clone();

        let observed = wait_for_gossip(&joiner, "alice-two", "hi bob").await;
        assert_eq!(
            observed,
            Some(sent_id),
            "joiner should receive message body=hi bob from alice-two with matching id"
        );

        // And the reverse direction.
        let reply_id = joiner
            .core
            .broadcast(MessageBody::from("hi alice"))
            .await
            .expect("a live session accepts a broadcast while the mesh is up")
            .id
            .clone();
        let observed2 = wait_for_gossip(&creator, "bob-two", "hi alice").await;
        assert_eq!(
            observed2,
            Some(reply_id),
            "the observed reply is the one sent"
        );

        joiner.leave().await;
        creator.leave().await;
    }

    #[tokio::test]
    async fn long_poll_blocks_then_returns_on_peer_traffic() {
        let creator = Session::create(create_cfg("lp", "alice-lp"))
            .await
            .expect("a loopback create needs no network and cannot fail");
        let mesh = creator.core.mesh_id().clone();
        let joiner = Session::join(join_cfg(&mesh, "bob-lp"))
            .await
            .expect("the creator is alive and its mesh id is valid, so a loopback join succeeds");

        // Mesh first (a delivered message proves the link) so the long-poll
        // below is waiting on a *fresh* event, not racing initial bootstrap.
        establish_link(&creator, &joiner, "alice-lp").await;

        // Baseline the joiner's cursor past the warmup, then block on a fetch
        // with a generous wait while the creator sends after a short delay.
        // `tokio::join!` runs both concurrently without `'static` (so neither
        // session needs cloning).
        let after = joiner
            .fetch_messages(None, false)
            .await
            .expect("baseline")
            .events
            .last()
            .map(|item| item.seq);

        let started = tokio::time::Instant::now();
        let delayed_send = async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            creator
                .core.broadcast(MessageBody::from("after warmup"))
                .await
                .expect("the sender is still joined, so the broadcast that wakes the poller must go out");
        };
        // The blocking fetch should resolve as soon as the gossiped "after
        // warmup" lands — well under the park cap — not spin to the timeout. A
        // single long-poll may return before gossip propagation completes, so
        // re-issue until the message shows (each call still parks). The park is
        // the daemon's 60s long-poll cap, so under a loaded host a correct
        // delivery still arrives long before the call would time out empty.
        let watch = tokio::time::timeout(
            Duration::from_secs(90),
            watch_for_body(&joiner, after, "after warmup"),
        );
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
    async fn send_broadcast_returns_full_echo_and_surfaces_self() {
        // send_broadcast returns an authoritative echo (id, author, ts, body)
        // so callers don't need to re-fetch to see their own send. With
        // stream-parity, the self-echo also surfaces once in a later fetch
        // tagged `self:true` — the same as the live `--output json` stream.
        let alice = Session::create(create_cfg("replay", "alice-replay"))
            .await
            .expect("a loopback create needs no network and cannot fail");

        let echo = alice
            .core
            .broadcast(MessageBody::from("self-echo"))
            .await
            .expect("a live session accepts a broadcast while the mesh is up");
        let sent = echo.id.clone();
        assert_eq!(
            echo.author.as_str(),
            "alice-replay",
            "the echo carries the author nickname"
        );
        assert_eq!(
            chat_text(&echo).as_deref(),
            Some("self-echo"),
            "the echo must carry the body that was sent, or the caller cannot match it to its own message"
        );
        assert!(
            echo.timestamp > habilis_network::protocol::UnixSecs::ZERO,
            "echo must carry a unix timestamp"
        );

        // The self-send surfaces in a fetch, marked `self:true`.
        let deadline = tokio::time::Instant::now() + DELIVER;
        let saw_self = poll_until(deadline, Duration::from_millis(150), || async {
            let events = alice
                .fetch_messages(None, false)
                .await
                .expect("a live session returns its buffered messages without error")
                .events;
            events
                .iter()
                .any(|item| is_self_echo(&item.event, &sent))
                .then_some(())
        })
        .await
        .is_some();
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
            .expect("a loopback create needs no network and cannot fail");
        let mesh = alice.core.mesh_id().clone();
        let bob = Session::join(join_cfg(&mesh, "bob-cursor"))
            .await
            .expect("the creator is alive and its mesh id is valid, so a loopback join succeeds");

        // Drive alice's first cursor-less fetches until bob's join presence has landed. The fetches
        // advance the implicit cursor past everything currently buffered — which is exactly what we
        // test next. Capture the seq just BEFORE bob's join (the prior event's seq) so a later
        // explicit replay from there re-reads bob's join + send.
        let deadline = tokio::time::Instant::now() + DELIVER;
        let replay_from = poll_until(deadline, Duration::from_millis(150), || async {
            let events = alice
                .fetch_messages(None, false)
                .await
                .expect("first fetch")
                .events;
            seq_before_bob_join(&events)
        })
        .await
        .expect("first fetches should see bob's join presence");

        // Second fetch with no new traffic: cursor advanced past everything
        // buffered, so the delta must be empty.
        let empty_delta = alice
            .fetch_messages(None, false)
            .await
            .expect("the cursor from the first fetch is valid, so the second fetch returns only newer messages")
            .events;
        assert!(
            empty_delta.is_empty(),
            "second cursor-less fetch must return delta (empty), got {empty_delta:?}"
        );

        // Bob sends — alice's next cursor-less fetch must surface bob's
        // message, nothing older.
        bob.core
            .broadcast(MessageBody::from("hi via cursor"))
            .await
            .expect("a live session accepts a broadcast while the mesh is up");
        let delta_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let saw_body = poll_until(delta_deadline, Duration::from_millis(150), || async {
            let events = alice
                .fetch_messages(None, false)
                .await
                .expect("delta fetch 2")
                .events;
            let mut messages = events.iter().filter_map(|item| as_message(&item.event));
            messages
                .any(|message| chat_text(message).as_deref() == Some("hi via cursor"))
                .then_some(())
        })
        .await
        .is_some();
        assert!(
            saw_body,
            "delta fetch after bob's send should surface body=hi via cursor"
        );

        // Explicit `after` is a non-mutating replay from an earlier seq: it must
        // re-surface bob's message regardless of where the implicit cursor sits.
        let forced = alice
            .fetch_messages(Some(replay_from), false)
            .await
            .expect("explicit fetch")
            .events;
        assert!(
            forced
                .iter()
                .filter_map(|item| as_message(&item.event))
                .any(|message| chat_text(message).as_deref() == Some("hi via cursor")),
            "explicit after must replay from the given seq"
        );

        // And the explicit replay must NOT have disturbed the implicit cursor:
        // a following cursor-less fetch sees no new traffic (empty).
        let after_replay = alice
            .fetch_messages(None, false)
            .await
            .expect("post-replay fetch")
            .events;
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
            .expect("a fresh process has no session yet, so the first create is not blocked");
        let first_mesh = first.core.mesh_id().clone();
        first.leave().await;

        // Second cycle — new session, new mesh.
        let second = Session::create(create_cfg("cy-b", "cycler-b"))
            .await
            .expect("second create after first was left");
        assert_ne!(
            second.core.mesh_id(),
            &first_mesh,
            "second create should mint a fresh mesh id"
        );
        assert_eq!(
            second.core.nickname().as_str(),
            "cycler-b",
            "the second session reports its own nickname"
        );
        second.leave().await;
    }

    /// An object merge applies and is reflected by `state_get` under the own author key; a
    /// non-object top-level merge is rejected, because the slice of an author is always a map.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn apply_state_merge_applies_and_reads_back() {
        let session = Session::create(create_cfg("merge", "alice"))
            .await
            .expect("a loopback create needs no network and cannot fail");
        let own = session.own_key().to_string();

        session
            .core
            .state_merge(json!({"turn": "a"}))
            .await
            .expect("an object merge applies");
        let expected = json!({ own.as_str(): {"turn": "a"} });
        assert_eq!(
            session
                .core
                .state_get()
                .await
                .expect("a created session owns its shared state document, so reading it succeeds"),
            expected,
            "the object merge sets turn to a in the own slice"
        );

        assert!(
            session.core.state_merge(json!([1, 2, 3])).await.is_err(),
            "a non-object top-level merge must be rejected"
        );
        assert_eq!(
            session
                .core
                .state_get()
                .await
                .expect("a created session owns its shared state document, so reading it succeeds"),
            expected,
            "a refused merge leaves the prior document"
        );

        session.leave().await;
    }

    #[test]
    fn a_failed_own_key_lookup_is_a_setup_error_not_a_session_without_a_key() {
        let error = super::require_own_key(Err(crate::api::SessionError::Stopped));
        assert!(
            error.is_err(),
            "a session whose key lookup failed must not be built without its key"
        );
    }

    #[test]
    fn an_own_key_that_resolved_passes_through() {
        let key = AuthorKey::from_bytes([7; 32]);
        assert_eq!(
            super::require_own_key(Ok(key)).ok(),
            Some(key),
            "a resolved key is handed back unchanged"
        );
    }
}
