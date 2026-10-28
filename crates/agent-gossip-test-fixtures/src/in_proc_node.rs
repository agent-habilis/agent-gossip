use std::path::Path;
use std::time::{Duration, Instant};

use agent_gossip::api::{
    A2aCallParams, CreateConfig, JoinConfig, MeshSession, TaskArtifactParams, TaskStatusParams,
};
use agent_gossip::{
    Channel, Lane, MessageBody, MessageId, MessageKind, Nickname, OutputEvent, PresenceSubtype,
    TaskId, TaskState,
};
use habilis_network::protocol::message::Authored;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::{MSG_TIMEOUT, POLL, RPC_ATTEMPT, init_test_tracing, is_rpc_timeout, test_mesh_name};

/// One in-process mesh node: a real [`MeshSession`] (real iroh
/// endpoint + the real `daemon::run` loop on a background task) plus
/// its captured [`OutputEvent`] stream. Drop-in analogue of the
/// subprocess [`Node`], but everything runs in the test process — so
/// coverage is recorded and teardown is deterministic.
pub struct InProcNode {
    pub session: MeshSession,
    pub(crate) rx: UnboundedReceiver<OutputEvent>,
    pub(crate) drained: Vec<OutputEvent>,
    pub mesh: String,
    pub nickname: String,
}

// `MeshSession` and the event receiver are not `Debug`; the identity plus how
// much has been captured is all a failing assertion needs anyway.
impl std::fmt::Debug for InProcNode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InProcNode")
            .field("mesh", &self.mesh)
            .field("nickname", &self.nickname)
            .field("drained", &self.drained.len())
            .finish_non_exhaustive()
    }
}

impl InProcNode {
    /// Create a new private mesh. `self.mesh` holds the id.
    pub async fn create(name: &str) -> Self {
        init_test_tracing();
        Self::from_session(
            MeshSession::create(CreateConfig::new(test_mesh_name(name)))
                .await
                .expect("in-process create failed"),
        )
    }

    /// Create a new private mesh with an explicit nickname.
    pub async fn create_with_nick(name: &str, nick: &str) -> Self {
        init_test_tracing();
        let mut cfg = CreateConfig::new(test_mesh_name(name));
        cfg.nickname = Some(Nickname::new(nick).expect("valid test nickname"));
        Self::from_session(
            MeshSession::create(cfg)
                .await
                .expect("in-process create failed"),
        )
    }

    /// Create a new private, password-protected mesh.
    pub async fn create_with_password(name: &str, password: &str) -> Self {
        init_test_tracing();
        let mut cfg = CreateConfig::new(test_mesh_name(name));
        cfg.password = Some(password.to_owned());
        Self::from_session(
            MeshSession::create(cfg)
                .await
                .expect("in-process passworded create failed"),
        )
    }

    /// Create a mesh under an explicit active-view cap (a small cap forces a
    /// partial mesh, so directed traffic must reach non-neighbours).
    pub async fn create_with_peers(name: &str, nick: &str, max_peers: usize) -> Self {
        init_test_tracing();
        let mut cfg = CreateConfig::new(test_mesh_name(name));
        cfg.nickname = Some(Nickname::new(nick).expect("valid test nickname"));
        cfg.max_peers = max_peers;
        Self::from_session(
            MeshSession::create(cfg)
                .await
                .expect("in-process transport create failed"),
        )
    }

    /// Join `mesh` under an explicit active-view cap.
    pub async fn join_with_peers(mesh: &str, nick: &str, max_peers: usize) -> Self {
        init_test_tracing();
        let target = mesh.parse().expect("valid test join target");
        let mut cfg = JoinConfig::new(target);
        cfg.nickname = Some(Nickname::new(nick).expect("valid test nickname"));
        cfg.max_peers = max_peers;
        Self::from_session(
            MeshSession::join(cfg)
                .await
                .expect("in-process transport join failed"),
        )
    }

    fn from_session(mut session: MeshSession) -> Self {
        let rx = session.events().expect("events() receiver");
        let mesh = session.mesh_id().to_string();
        let nickname = session.nickname().to_string();
        Self {
            session,
            rx,
            drained: Vec::new(),
            mesh,
            nickname,
        }
    }

    /// Join `mesh` (a mesh id) with an explicit nickname.
    pub async fn join(mesh: &str, nickname: &str) -> Self {
        init_test_tracing();
        let target = mesh.parse().expect("valid test join target");
        let mut cfg = JoinConfig::new(target);
        cfg.nickname = Some(Nickname::new(nickname).expect("valid test nickname"));
        Self::from_session(
            MeshSession::join(cfg)
                .await
                .expect("in-process join failed"),
        )
    }

    /// Join the public topic mesh derived from `string`, with an explicit
    /// nickname.
    pub async fn topic(string: &str, nickname: &str) -> Self {
        init_test_tracing();
        let mut cfg = agent_gossip::api::TopicConfig::new(string.to_owned());
        cfg.nickname = Some(Nickname::new(nickname).expect("valid test nickname"));
        Self::from_session(
            MeshSession::topic(cfg)
                .await
                .expect("in-process topic join failed"),
        )
    }

    /// Join a password-protected `mesh`, surfacing the join error (so a
    /// wrong-password test can assert on it).
    pub async fn try_join_with_password(
        mesh: &str,
        nickname: &str,
        password: &str,
    ) -> Result<Self, agent_gossip::api::JoinError> {
        let target = mesh.parse().expect("valid test join target");
        let mut cfg = JoinConfig::new(target);
        cfg.nickname = Some(Nickname::new(nickname).expect("valid test nickname"));
        cfg.password = Some(password.to_owned());
        Ok(Self::from_session(MeshSession::join(cfg).await?))
    }

    /// Broadcast a plain message to everyone; returns the new message id.
    pub async fn broadcast(&self, text: &str) -> MessageId {
        self.session
            .broadcast(MessageBody::new(text).expect("valid body"))
            .await
            .expect("in-process broadcast failed")
            .id
            .clone()
    }

    /// Send a msg to one peer; returns the new message id.
    pub async fn msg(&self, to: &str, text: &str) -> MessageId {
        self.try_msg(to, text)
            .await
            .expect("in-process msg failed")
            .id
            .clone()
    }

    /// [`Self::msg`] without the panic — a msg is sealed to the recipient, so
    /// it legitimately fails until that peer's card has replicated, and a test
    /// that races the card needs to see the error rather than die on it.
    ///
    /// # Errors
    /// Propagates the send failure (loop stopped, or the recipient's seal key
    /// is not known yet).
    pub async fn try_msg(&self, to: &str, text: &str) -> anyhow::Result<Authored> {
        self.session
            .msg(
                Nickname::new(to).expect("valid test nickname"),
                MessageBody::new(text).expect("valid body"),
            )
            .await
    }

    /// Apply an RFC 7386 merge to the shared state. Panics if the merge is
    /// rejected (not a JSON object / loop stopped).
    pub async fn state_merge(&self, merge: serde_json::Value) {
        self.try_state_merge(merge)
            .await
            .expect("in-process state_merge failed");
    }

    /// Like [`Self::state_merge`] but returns the raw result.
    async fn try_state_merge(&self, merge: serde_json::Value) -> anyhow::Result<()> {
        self.session.state_merge(merge).await
    }

    /// The current derived shared-state document (the merge fold over the
    /// state log).
    pub async fn state_get(&self) -> serde_json::Value {
        self.session
            .state_get()
            .await
            .expect("in-process state_get failed")
    }

    /// Apply an RFC 7386 merge to the `meta` channel. Panics on rejection —
    /// use [`Self::try_meta_merge`] to exercise rejection deliberately.
    pub async fn meta_merge(&self, merge: serde_json::Value) {
        self.try_meta_merge(merge)
            .await
            .expect("in-process meta_merge failed");
    }

    /// Like [`Self::meta_merge`] but returns the raw result.
    pub async fn try_meta_merge(&self, merge: serde_json::Value) -> anyhow::Result<()> {
        self.session.meta_merge(merge).await
    }

    /// The current derived `meta`-channel document.
    pub async fn meta_get(&self) -> serde_json::Value {
        self.session
            .meta_get()
            .await
            .expect("in-process meta_get failed")
    }

    // ── channel-parameterized dispatch ──
    // The behavioral tests run against both channels by selecting one at the
    // call site; these forward to the `state_*` / `meta_*` twins above so a
    // single test body covers `Channel::State` and `Channel::Meta`.

    /// Apply a merge to `channel`. Panics on rejection.
    pub async fn merge(&self, channel: Channel, merge: serde_json::Value) {
        match channel {
            Channel::State => self.state_merge(merge).await,
            Channel::Meta => self.meta_merge(merge).await,
        }
    }

    /// The current derived document for `channel`.
    pub async fn get(&self, channel: Channel) -> serde_json::Value {
        match channel {
            Channel::State => self.state_get().await,
            Channel::Meta => self.meta_get().await,
        }
    }

    /// Captured changes on `channel` so far, each `(derived document, is_self)`.
    pub fn changes(&mut self, channel: Channel) -> Vec<(serde_json::Value, bool)> {
        self.pump();
        self.drained
            .iter()
            .filter_map(|event| match event {
                OutputEvent::StateChanged {
                    channel: chan,
                    document,
                    is_self,
                    ..
                } if *chan == channel => Some((document.clone(), *is_self)),
                _ => None,
            })
            .collect()
    }

    /// Wait until a change on `channel` **from a peer** (`is_self == false`)
    /// whose freshly-derived document satisfies `pred` is captured — the
    /// reaction-hook check (a self-change never satisfies it, exercising the F5
    /// self-wake guard).
    pub async fn wait_change(
        &mut self,
        channel: Channel,
        timeout: Duration,
        mut pred: impl FnMut(&serde_json::Value) -> bool,
    ) -> bool {
        self.wait_for(timeout, |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    OutputEvent::StateChanged { channel: chan, document, is_self: false, .. }
                        if *chan == channel && pred(document)
                )
            })
        })
        .await
    }

    /// Create a task on `target` (native A2A `message/send`, no `taskId`): the
    /// worker mints the id and returns the `Task`. Returns the parsed JSON-RPC
    /// response.
    pub async fn create_task(&self, target: &str, text: &str) -> serde_json::Value {
        self.create_task_labeled(target, text, None).await
    }

    /// As [`Self::create_task`], with the initiator's one-line task label
    /// (`mesh:label`) the worker is meant to reuse verbatim.
    pub async fn create_task_labeled(
        &self,
        target: &str,
        text: &str,
        label: Option<&str>,
    ) -> serde_json::Value {
        // Task creation seals the request to `target`'s card
        // (`a2a::card::peer_seal_key`). The card propagates asynchronously and the
        // seal is one-shot, so wait for it first — otherwise, under the concurrent
        // suite's load, the request is silently dropped and the call hangs.
        self.await_peer_card(target).await;
        let msg = agent_gossip::a2a::status_update_params::send_message_payload(
            self.session.mesh_id(),
            None,
            text,
            label,
        );
        self.a2a_call_retrying(target, "SendMessage", serde_json::json!({ "message": msg }))
            .await
    }

    /// Block until `target`'s card is present in this node's `meta` doc — the
    /// seal-key source for any directed A2A call, and the precondition
    /// [`Self::a2a_call`] cannot enforce itself (the unknown-peer tests call a
    /// nickname whose card will never arrive and must fail fast, not block).
    /// [`Self::create_task`] applies it for you; a bare `a2a_call` to a real
    /// peer must call this first or it races card propagation.
    ///
    /// The card is not enough on its own: the meta doc and the live roster
    /// converge separately, and a directed send checks the roster. Until
    /// `target` is in it on a lane that carries payload, the send fails as an
    /// unknown peer or is held for a relay that is lookup only — both seen as
    /// CI flakes in `tests/seal.rs`.
    pub async fn await_peer_card(&self, target: &str) {
        let pointer = format!("/peers/{target}/card");
        let deadline = Instant::now() + MSG_TIMEOUT;
        while self.meta_get().await.pointer(&pointer).is_none()
            || !self.carries_directed_frames_to(target).await
        {
            assert!(
                Instant::now() < deadline,
                "{target}'s card never reached this node's meta doc, or no direct lane to it formed"
            );
            tokio::time::sleep(POLL).await;
        }
    }

    async fn carries_directed_frames_to(&self, target: &str) -> bool {
        self.session.peers().await.is_ok_and(|roster| {
            roster.peers.iter().any(|entry| {
                entry.nickname.as_str() == target
                    && !entry.quiet
                    && matches!(entry.transport, Lane::Unicast | Lane::Multihop)
            })
        })
    }

    /// Send a follow-up message (answer / approval / change) into an existing
    /// task on `target`. Returns the updated `Task` response.
    pub async fn task_message(
        &self,
        target: &str,
        task_id: &TaskId,
        text: &str,
    ) -> serde_json::Value {
        let msg = agent_gossip::a2a::status_update_params::send_message_payload(
            self.session.mesh_id(),
            Some(task_id),
            text,
            None,
        );
        self.a2a_call_retrying(target, "SendMessage", serde_json::json!({ "message": msg }))
            .await
    }

    /// A directed A2A call to `target`, sent **exactly once**; returns the
    /// parsed JSON-RPC response.
    ///
    /// Reach for this only to assert on single-shot behaviour itself. Anything
    /// that merely needs the call to *land* wants [`Self::a2a_call_retrying`]:
    /// a directed request dropped by a momentarily linkless overlay is never
    /// retransmitted, so a lone attempt just burns the caller's deadline.
    pub async fn a2a_call(
        &self,
        target: &str,
        method: &str,
        params: serde_json::Value,
    ) -> serde_json::Value {
        self.session
            .a2a_call(A2aCallParams {
                peer: Nickname::new(target).expect("valid target"),
                method: method.to_owned(),
                params,
                timeout: MSG_TIMEOUT,
            })
            .await
            .expect("a2a_call transport")
    }

    /// A small directed A2A call, **re-issued while the peer never answers**.
    ///
    /// A directed request is broadcast into the gossip overlay, and `A2aReq` /
    /// `A2aResp` are unlogged — anti-entropy never heals them. So a request sent
    /// while the overlay happens to hold no live peer link is dropped *for good*
    /// and the caller simply blocks to its deadline. That window is real and
    /// routine right after a join: `linked_endpoints` is written only by
    /// `NeighborUp`/`NeighborDown`, so a fresh pair can link, deliver, and then
    /// watch the neighbor go down, leaving the daemon at "silent partition
    /// (meshed but zero peer links)" until the next heal tick re-bridges it 15s
    /// later. A single-shot call therefore races overlay convergence — this was
    /// the suite's long-standing ~50% flake in `a2a_rpc`, reproducible on one
    /// test with no concurrency at all.
    ///
    /// Retrying is the same remedy the subprocess unicast test already applies
    /// (`gossip_network.rs`, "retry until it lands"), and it is safe here: the
    /// task registry treats a duplicate leg as a no-op (`a2a/task.rs`). Each
    /// attempt gets a short deadline so a dropped frame is re-sent promptly
    /// rather than burning the budget waiting on a frame the overlay discarded —
    /// which is exactly why this must stay off the large-payload path, where a
    /// resend overflows the outbound queue instead. See [`Self::a2a_call`].
    ///
    /// Only a transport timeout is retried. Any genuine answer — including a
    /// semantic error such as "not permitted" or "unknown peer" — means
    /// the round trip completed, and is returned untouched.
    pub async fn a2a_call_retrying(
        &self,
        target: &str,
        method: &str,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let peer = Nickname::new(target).expect("valid target");
        let deadline = Instant::now() + MSG_TIMEOUT;
        loop {
            let response = self
                .session
                .a2a_call(A2aCallParams {
                    peer: peer.clone(),
                    method: method.to_owned(),
                    params: params.clone(),
                    timeout: RPC_ATTEMPT,
                })
                .await
                .expect("a2a_call transport");
            if !is_rpc_timeout(&response) || Instant::now() >= deadline {
                return response;
            }
        }
    }

    /// Worker-emit a `TaskStatusUpdate` on a task we're serving.
    pub async fn task_status(&self, task_id: &TaskId, state: TaskState, note: Option<&str>) {
        self.session
            .task_status(TaskStatusParams {
                task_id: task_id.clone(),
                state,
                note: note.map(str::to_owned),
                waiting: None,
            })
            .await
            .expect("in-process task_status failed");
    }

    /// Worker-emit a `working` status that says the worker waits on its user.
    pub async fn task_waiting(&self, task_id: &TaskId, reason: &str) {
        self.session
            .task_status(TaskStatusParams {
                task_id: task_id.clone(),
                state: TaskState::Working,
                note: None,
                waiting: Some(reason.to_owned()),
            })
            .await
            .expect("in-process task_waiting failed");
    }

    /// Worker-emit a `TaskArtifactUpdate` (the result) on a task we're serving.
    pub async fn task_artifact(&self, task_id: &TaskId, text: &str) {
        self.session
            .task_artifact(TaskArtifactParams {
                task_id: task_id.clone(),
                text: text.to_owned(),
                file: None,
                file_name: None,
                file_mime: None,
            })
            .await
            .expect("in-process task_artifact failed");
    }

    /// Worker-emit a `TaskArtifactUpdate` whose result is a file, offloaded over
    /// the blob channel and referenced by a `Part.url`. Returns the daemon's echo
    /// so a test can read the minted ticket reference.
    pub async fn task_artifact_file(&self, task_id: &TaskId, path: &Path) -> Authored {
        self.session
            .task_artifact(TaskArtifactParams {
                task_id: task_id.clone(),
                text: String::new(),
                file: Some(path.to_path_buf()),
                file_name: None,
                file_mime: None,
            })
            .await
            .expect("in-process task_artifact_file failed")
    }

    /// Captured worker-pushed task frames (status/artifact; includes self
    /// echoes — filter on `is_self`/author as needed).
    pub fn tasks(&mut self) -> Vec<(&Authored, bool)> {
        self.pump();
        self.drained
            .iter()
            .filter_map(|event| match event {
                OutputEvent::Task { msg, is_self } => Some((&**msg, *is_self)),
                _ => None,
            })
            .collect()
    }

    /// Whether any `message`-kind task event (an RPC `message/send` leg) has
    /// been surfaced so far.
    pub fn saw_task_message(&mut self) -> bool {
        self.pump();
        self.drained
            .iter()
            .any(|event| matches!(event, OutputEvent::TaskMessage { .. }))
    }

    /// Wait until a worker-pushed task frame in `state` (from any author) is
    /// surfaced.
    pub async fn wait_task_state(&mut self, state: TaskState, timeout: Duration) -> bool {
        self.wait_for(timeout, |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    OutputEvent::Task { msg, .. }
                        if agent_gossip::a2a::status_update_params::frame_task_state(msg) == Some(state)
                )
            })
        })
        .await
    }

    /// Wait until a `message`-kind task event (an RPC `message/send` leg) is
    /// surfaced to us (the worker).
    pub async fn wait_task_message(&mut self, timeout: Duration) -> bool {
        self.wait_for(timeout, |events| {
            events
                .iter()
                .any(|event| matches!(event, OutputEvent::TaskMessage { .. }))
        })
        .await
    }

    /// The `mesh:label` on the first `message`-kind task event surfaced to us —
    /// `Some(None)` when that leg carried no label, `None` when no such leg has
    /// been surfaced at all.
    pub fn task_message_label(&mut self) -> Option<Option<String>> {
        self.pump();
        self.drained.iter().find_map(|event| match event {
            OutputEvent::TaskMessage { label, .. } => Some(label.clone()),
            _ => None,
        })
    }

    /// Clean shutdown (broadcasts `Left`).
    pub async fn leave(self) {
        let _ = self.session.leave().await;
    }

    /// Non-blocking: move every pending captured event into the buffer.
    fn pump(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            self.drained.push(event);
        }
    }

    /// Every captured event so far (drains pending first).
    pub fn events(&mut self) -> &[OutputEvent] {
        self.pump();
        &self.drained
    }

    /// Captured events rendered to the documented `--output json`
    /// wire format and parsed — byte-identical to what the subprocess
    /// emitted.
    pub fn json_events(&mut self) -> Vec<serde_json::Value> {
        self.pump();
        self.drained
            .iter()
            .filter_map(agent_gossip::event_json)
            .filter_map(|line| serde_json::from_str(&line).ok())
            .collect()
    }

    /// Only `{"event":"message"}` lines (both `msg` and `presence`
    /// carry `event:"message"`).
    pub fn message_events(&mut self) -> Vec<serde_json::Value> {
        self.json_events()
            .into_iter()
            .filter(|value| value["event"] == "message")
            .collect()
    }

    /// `{"event":"message","type":"broadcast"}` lines only — gossip-wide chat,
    /// excluding presence and msgs.
    pub fn broadcast_events(&mut self) -> Vec<serde_json::Value> {
        self.chat_events("broadcast")
    }

    /// `{"event":"message","type":"msg"}` lines only — chat addressed to one
    /// peer.
    pub fn msg_events(&mut self) -> Vec<serde_json::Value> {
        self.chat_events("msg")
    }

    fn chat_events(&mut self, ty: &str) -> Vec<serde_json::Value> {
        self.json_events()
            .into_iter()
            .filter(|value| value["event"] == "message" && value["type"] == ty)
            .collect()
    }

    /// Inbound `msg`-kind messages captured so far (includes self
    /// echoes — filter on `is_self` if needed).
    pub fn messages(&mut self) -> Vec<&Authored> {
        self.pump();
        self.drained
            .iter()
            .filter_map(|event| match event {
                OutputEvent::Message { msg, .. } => Some(&**msg),
                _ => None,
            })
            .collect()
    }

    /// `msg` events from *other* peers only (drops our own echoes).
    /// The subprocess [`Node`] parsed only peer lines, so ports that
    /// counted "deliveries" use this.
    pub fn inbound(&mut self) -> Vec<&Authored> {
        self.pump();
        self.drained
            .iter()
            .filter_map(|event| match event {
                OutputEvent::Message {
                    msg,
                    is_self: false,
                } => Some(&**msg),
                _ => None,
            })
            .collect()
    }

    /// Wait until a peer (non-self) `msg` with exactly `body` is
    /// captured, or `timeout` elapses.
    pub async fn wait_body(&mut self, body: &str, timeout: Duration) -> bool {
        self.wait_for(timeout, |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    OutputEvent::Message { msg, is_self: false }
                        if agent_gossip::a2a::status_update_params::chat_text(msg).as_deref() == Some(body)
                )
            })
        })
        .await
    }

    /// Wait until at least `min_count` peer (non-self) messages have
    /// been captured, or `timeout` elapses.
    pub async fn wait_inbound(&mut self, min_count: usize, timeout: Duration) -> bool {
        self.wait_for(timeout, |events| {
            events
                .iter()
                .filter(|event| matches!(event, OutputEvent::Message { is_self: false, .. }))
                .count()
                >= min_count
        })
        .await
    }

    /// Count inbound (non-self) `msg` events whose body equals `body`.
    pub fn count_body(&mut self, body: &str) -> usize {
        self.pump();
        self.drained
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    OutputEvent::Message { msg, is_self: false }
                        if agent_gossip::a2a::status_update_params::chat_text(msg).as_deref() == Some(body)
                )
            })
            .count()
    }

    /// Number of surfaced presence events of the given kind.
    pub fn presence_count(&mut self, want: PresenceSubtype) -> usize {
        self.pump();
        self.drained
            .iter()
            .filter(|event| match event {
                OutputEvent::Presence { msg } => {
                    matches!(&msg.kind, MessageKind::Presence { subtype } if *subtype == want)
                }
                _ => false,
            })
            .count()
    }

    /// Wait until a `joined`/`left` presence for `nick` is surfaced.
    pub async fn wait_presence(
        &mut self,
        nick: &str,
        want: PresenceSubtype,
        timeout: Duration,
    ) -> bool {
        self.wait_for(timeout, |events| {
            events.iter().any(|event| match event {
                OutputEvent::Presence { msg } => {
                    msg.author.to_string() == nick
                        && matches!(&msg.kind, MessageKind::Presence { subtype } if *subtype == want)
                }
                _ => false,
            })
        })
        .await
    }

    /// Wait until at least `min_count` `joined`/`left` presence events
    /// (any author) have been surfaced.
    pub async fn wait_presence_count(
        &mut self,
        want: PresenceSubtype,
        min_count: usize,
        timeout: Duration,
    ) -> bool {
        self.wait_for(timeout, |events| {
            events
                .iter()
                .filter(|event| match event {
                    OutputEvent::Presence { msg } => {
                        matches!(&msg.kind, MessageKind::Presence { subtype } if *subtype == want)
                    }
                    _ => false,
                })
                .count()
                >= min_count
        })
        .await
    }

    /// Poll until `pred` over the accumulated events holds, or
    /// `timeout` elapses. Returns whether the predicate was satisfied.
    pub async fn wait_for(
        &mut self,
        timeout: Duration,
        mut pred: impl FnMut(&[OutputEvent]) -> bool,
    ) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            self.pump();
            if pred(&self.drained) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Convenience: wait until at least `n` inbound `msg` events are
    /// seen (any author, including self echoes).
    pub async fn wait_messages(&mut self, min_count: usize, timeout: Duration) -> bool {
        self.wait_for(timeout, |events| {
            events
                .iter()
                .filter(|event| matches!(event, OutputEvent::Message { .. }))
                .count()
                >= min_count
        })
        .await
    }
}
