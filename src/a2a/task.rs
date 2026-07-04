//! The daemon-side **task** state machine + its two timers, over the A2A
//! lifecycle.
//!
//! A task is a directed, multi-leg conversation whose wire form is pure A2A:
//! a `message/send` (no `taskId`) opens it — the worker mints the id and
//! returns the `Task` — then `TaskStatusUpdate`s and mid-task `message/send`s
//! advance it, a `TaskArtifactUpdate` returns the result, and the **worker's**
//! `completed` status closes it (after the initiator's approval message). The
//! daemon owns only the *coarse* lifecycle — state advance, the per-task idle
//! debounce, the ball-owner keepalive, and the content-message cap — while the
//! skill owns the *content*.
//!
//! The machine is **distributed** with no consensus: each party derives
//! state from the legs it has seen, so the rules are deliberately
//! conservative — **monotonic** advance (a leg that would move backward is
//! ignored), **idempotent** (a duplicate leg is a no-op), and a terminal
//! record is frozen. Local triggers (timeout, cap) *broadcast* a terminal
//! `canceled` status so the other side converges.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use iroh_gossip::api::GossipSender;

use crate::daemon::state::EventLoopState;
use crate::output;
use crate::protocol::{Message, MessageKind, Nickname, SwarmId};
use crate::util::consts::TASK_CONTENT_CAP;
use crate::util::tuning::{task_keepalive_max_secs, task_keepalive_secs, task_timeout_secs};

use super::{META_REASON, TaskId, TaskState, gossip};

/// My part in a task: did I open it (client side), or receive the offer
/// (the worker — the task's A2A server)?
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TaskRole {
    Initiator,
    Receiver,
}

impl TaskRole {
    fn opposite(self) -> Self {
        match self {
            TaskRole::Initiator => TaskRole::Receiver,
            TaskRole::Receiver => TaskRole::Initiator,
        }
    }
}

/// One in-flight task this node is a party to.
#[derive(Clone, Debug)]
pub(crate) struct TaskRecord {
    /// The other party.
    pub peer: Nickname,
    pub role: TaskRole,
    /// The A2A lifecycle state, as derived from the legs seen so far.
    /// `Submitted` is the not-yet-accepted opening; terminal states freeze
    /// the record.
    pub state: TaskState,
    /// Local UX hint: the task is parked in `input-required` because an
    /// artifact arrived (awaiting the initiator's approval), rather than
    /// because the worker asked a question. Derived from the artifact leg — no
    /// wire marker.
    pub review: bool,
    /// Local-clock instant of the last leg (inbound or our own, **including**
    /// the daemon's own keepalive) — the idle-debounce reads this, never the
    /// wire `ts` (which can skew).
    pub last_activity: Instant,
    /// Local-clock instant of the last leg driven by a **skill** on either
    /// side — every real leg through [`apply`], but **not** the daemon's own
    /// keepalive (which never routes through [`apply`]). The keepalive gates
    /// on this, not `last_activity`: reading the same clock the keepalive
    /// refreshes would let it feed the timeout it is subject to, so a crashed
    /// skill would keepalive the peer forever. See [`should_keepalive`].
    pub last_skill_activity: Instant,
    /// Count of **content** legs seen (beats excluded) — the cap.
    pub content_count: u32,
    /// Last progress fraction reported, replayed on keepalives.
    pub last_fraction: Option<(u64, u64)>,
    /// Which role currently owes the next move — that party's daemon emits
    /// the keepalive, and that party's silence is what eventually times out.
    pub ball: TaskRole,
}

impl TaskRecord {
    /// Am I the ball-owner (so my daemon keepalives this task)?
    fn i_own_ball(&self) -> bool {
        self.ball == self.role
    }

    /// Should my daemon emit a keepalive for this task right now? True only
    /// when I own the ball on a live task, I have been quiet past the
    /// keepalive `cadence`, **and** a skill has driven a real leg within
    /// `max_silence` — the last gate is what stops a crashed skill's daemon
    /// from keepaliving the peer forever (the keepalive itself never
    /// refreshes `last_skill_activity`, so once the skill stops, this goes
    /// false and the peer's debounce reaps the task).
    fn should_keepalive(&self, now: Instant, cadence: Duration, max_silence: Duration) -> bool {
        !self.state.is_terminal()
            && self.i_own_ball()
            && now.duration_since(self.last_activity) >= cadence
            && now.duration_since(self.last_skill_activity) < max_silence
    }
}

/// What one applied leg *is*, reduced from its wire form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LegKind {
    /// The task-creating `message/send` (no `taskId`) — the worker mints the id
    /// and opens the record; the initiator adopts it from the RPC response.
    Offer,
    /// A mid-task `message/send` carrying a `taskId` (the initiator's answer /
    /// approval / change request, applied from the RPC path).
    Text,
    /// A status transition to `state` (never a beat).
    Status(TaskState),
    /// The worker's result — parks the task in review.
    Artifact,
    /// A liveness beat (keepalive / progress plumbing).
    Beat,
}

/// The fields of an applied leg the state machine needs.
pub(crate) struct LegInfo<'a> {
    pub task_id: &'a TaskId,
    /// The other party (inbound: the author; outbound: the `to`).
    pub peer: &'a Nickname,
    pub kind: LegKind,
    /// `true` if we sent this leg (outbound echo), `false` if it arrived.
    pub mine: bool,
    /// `done/total` carried by a beat (else `None`).
    pub fraction: Option<(u64, u64)>,
}

/// Feed one **validated logical** task frame into the registry from the
/// broadcast (`mine = true`) or receive (`mine = false`) path. The single
/// call site for both: it reduces the frame to a [`LegInfo`] (deriving the
/// peer and the beat fraction) and advances the machine. A frame that
/// belongs to no task is a no-op. Returns `true` if this leg crossed the
/// content cap.
pub(crate) fn ingest(
    tasks: &mut HashMap<TaskId, TaskRecord>,
    frame: &Message,
    mine: bool,
    now: Instant,
) -> bool {
    let (to, kind, fraction, task_id) = match &frame.kind {
        MessageKind::A2aStatus { to, task_id } => {
            let Ok(payload) = gossip::status_payload(frame) else {
                return false;
            };
            if gossip::is_beat(&payload) {
                (
                    to,
                    LegKind::Beat,
                    gossip::beat_fraction(&payload),
                    task_id.clone(),
                )
            } else {
                (
                    to,
                    LegKind::Status(payload.status.state),
                    None,
                    task_id.clone(),
                )
            }
        }
        MessageKind::A2aArtifact { to, task_id } => (to, LegKind::Artifact, None, task_id.clone()),
        // `A2aMsg` is swarm broadcast chat, never a task leg. Task legs arrive
        // as status/artifact push (above) or are applied directly from the
        // RPC `message/send` path (`Offer`/`Text`), not through `ingest`.
        MessageKind::A2aMsg
        | MessageKind::Presence { .. }
        | MessageKind::PeerInfo
        | MessageKind::Digest
        | MessageKind::StateDigest
        | MessageKind::MetaDigest
        | MessageKind::Ping
        | MessageKind::Pong { .. }
        | MessageKind::A2aReq { .. }
        | MessageKind::A2aResp { .. }
        | MessageKind::State
        | MessageKind::Meta
        | MessageKind::LinkState => return false,
    };
    let peer = if mine { to } else { &frame.author };
    apply(
        tasks,
        &LegInfo {
            task_id: &task_id,
            peer,
            kind,
            mine,
            fraction,
        },
        now,
    )
}

/// Upsert the **initiator**-side record for a task whose authoritative `Task`
/// the worker just returned over RPC (`message/send` create/follow-up). The
/// worker (server) is the source of truth for the task's state, so we mirror
/// `task_state` rather than replay legs. Terminal records are frozen.
pub(crate) fn adopt_initiator(
    tasks: &mut HashMap<TaskId, TaskRecord>,
    task_id: &TaskId,
    peer: &Nickname,
    task_state: TaskState,
    now: Instant,
) {
    match tasks.entry(task_id.clone()) {
        std::collections::hash_map::Entry::Vacant(slot) => {
            // First adoption (task creation): open the initiator-side record at
            // the server-minted state.
            slot.insert(TaskRecord {
                peer: peer.clone(),
                role: TaskRole::Initiator,
                state: task_state,
                review: false,
                last_activity: now,
                last_skill_activity: now,
                content_count: 0,
                last_fraction: None,
                // Whoever owes the next move: the worker on a live task, us if
                // it parked for our input.
                ball: if task_state == TaskState::InputRequired {
                    TaskRole::Initiator
                } else {
                    TaskRole::Receiver
                },
            });
        }
        std::collections::hash_map::Entry::Occupied(mut slot) => {
            // Already tracking. The worker-pushed status/artifact stream is the
            // authoritative source of *live* transitions, so a late or reordered
            // RPC snapshot (e.g. a `GetTask` reply that raced the artifact push)
            // must NOT regress a live non-terminal state. But a **terminal**
            // snapshot is final truth — it repairs a terminal frame we missed
            // (aged out of the anti-entropy window), the reconciliation path a
            // manual `GetTask` gives.
            let rec = slot.get_mut();
            if rec.state.is_terminal() {
                return;
            }
            rec.last_activity = now;
            if task_state.is_terminal() {
                rec.state = task_state;
                rec.review = false;
                rec.ball = TaskRole::Receiver;
            }
        }
    }
}

/// Apply one task leg to the registry, advancing the coarse machine.
/// Monotonic + idempotent + terminal-frozen. Returns `true` if this leg
/// pushed the content count **over** the cap (the daemon warns once).
pub(crate) fn apply(
    tasks: &mut HashMap<TaskId, TaskRecord>,
    leg: &LegInfo<'_>,
    now: Instant,
) -> bool {
    // A terminal record is immutable — late/duplicate legs are ignored.
    if tasks
        .get(leg.task_id)
        .is_some_and(|rec| rec.state.is_terminal())
    {
        return false;
    }

    if matches!(leg.kind, LegKind::Offer) {
        // The opening leg mints the record; a duplicate offer just touches it.
        tasks
            .entry(leg.task_id.clone())
            .or_insert_with(|| TaskRecord {
                peer: leg.peer.clone(),
                role: if leg.mine {
                    TaskRole::Initiator
                } else {
                    TaskRole::Receiver
                },
                state: TaskState::Submitted,
                review: false,
                last_activity: now,
                last_skill_activity: now,
                content_count: 0,
                last_fraction: None,
                ball: TaskRole::Receiver,
            });
    }

    let Some(rec) = tasks.get_mut(leg.task_id) else {
        // A non-offer leg for a task we never saw open — drop it (out of
        // order, or a task that began before our join horizon).
        return false;
    };

    advance(rec, leg.kind, leg.mine);
    if let Some(fraction) = leg.fraction {
        rec.last_fraction = Some(fraction);
    }
    rec.last_activity = now;
    // A real leg (skill-sent or peer-received) proves a skill is driving this
    // task; the daemon's own keepalive never routes through `apply`, so it
    // cannot refresh this clock and thus cannot cover for a dead skill forever.
    rec.last_skill_activity = now;

    // Content legs (everything but the plumbing beat) burn the budget.
    let mut over_cap = false;
    if !matches!(leg.kind, LegKind::Beat) {
        rec.content_count = rec.content_count.saturating_add(1);
        over_cap = rec.content_count == TASK_CONTENT_CAP + 1;
    }
    over_cap
}

/// The per-leg coarse transition (state + review + ball). Illegal /
/// out-of-order legs for the current state are silently ignored (the
/// conservative, no-consensus rule). Direction matters: only the worker
/// advances into `working` / `input-required` / `rejected` / `failed` **and
/// closes the task with `completed`** (native A2A — the server drives its own
/// task to terminal); the initiator only sends messages (answers / approval /
/// change requests). `canceled` is open to both (and to the daemon's own
/// timeout).
fn advance(rec: &mut TaskRecord, kind: LegKind, mine: bool) {
    let sender = if mine { rec.role } else { rec.role.opposite() };
    match kind {
        LegKind::Status(TaskState::Working)
            if sender == TaskRole::Receiver
                && matches!(rec.state, TaskState::Submitted | TaskState::InputRequired) =>
        {
            // The worker commits (accept) or resumes; it keeps the ball.
            rec.state = TaskState::Working;
            rec.review = false;
            rec.ball = TaskRole::Receiver;
        }
        LegKind::Status(TaskState::InputRequired)
            if sender == TaskRole::Receiver
                && matches!(rec.state, TaskState::Submitted | TaskState::Working) =>
        {
            // The worker asks a question → the initiator owes the answer.
            rec.state = TaskState::InputRequired;
            rec.review = false;
            rec.ball = TaskRole::Initiator;
        }
        LegKind::Text => {
            // A message into the task (the initiator's answer / approval /
            // change request, or the worker's own note): flip the ball to
            // whoever now owes the reply. An initiator's message resolves an
            // `input-required` park — the worker resumes and decides whether to
            // complete (approval) or rework (change); the skill reads the text.
            if sender == TaskRole::Initiator && rec.state == TaskState::InputRequired {
                rec.state = TaskState::Working;
                rec.review = false;
            }
            rec.ball = sender.opposite();
        }
        LegKind::Artifact
            if sender == TaskRole::Receiver
                && matches!(rec.state, TaskState::Submitted | TaskState::Working) =>
        {
            // The result arrived → park for the initiator's review. (From
            // `Submitted` too: a worker may run a trivial task without an
            // explicit accept first.)
            rec.state = TaskState::InputRequired;
            rec.review = true;
            rec.ball = TaskRole::Initiator;
        }
        LegKind::Status(TaskState::Completed) if sender == TaskRole::Receiver => {
            // Native A2A: the worker (server) drives its own task to
            // `completed`, after the initiator's approval message (or directly,
            // for a walk-away handover).
            rec.state = TaskState::Completed;
        }
        LegKind::Status(TaskState::Rejected)
            if sender == TaskRole::Receiver
                && matches!(rec.state, TaskState::Submitted | TaskState::Working) =>
        {
            rec.state = TaskState::Rejected;
        }
        LegKind::Status(TaskState::Failed) if sender == TaskRole::Receiver => {
            rec.state = TaskState::Failed;
        }
        LegKind::Status(TaskState::Canceled) => rec.state = TaskState::Canceled,
        // No-op here: `Offer` is handled at record creation, `Beat` is
        // liveness-only, and every other combination lands here only when
        // its guard above failed (an out-of-order/backward/misdirected leg,
        // conservatively ignored).
        LegKind::Offer
        | LegKind::Beat
        | LegKind::Artifact
        | LegKind::Status(
            TaskState::Submitted
            | TaskState::Working
            | TaskState::InputRequired
            | TaskState::Completed
            | TaskState::Rejected
            | TaskState::Failed
            | TaskState::AuthRequired
            | TaskState::Unspecified,
        ) => {}
    }
}

/// Sweep the registry for tasks idle past the debounce timeout, then
/// **garbage-collect** records that have been terminal longer than the
/// timeout. Each eviction freezes the record, emits a `task_timeout` event,
/// and broadcasts a terminal `canceled` status (with the timeout reason in
/// metadata) so the peer converges; the GC pass keeps `state.tasks` bounded
/// (a terminal record older than the timeout is past the dedup window — no
/// further leg for that task will arrive, so it is safe to drop). The task
/// analogue of [`crate::lifecycle::heartbeat::tick_sweep`].
pub(crate) async fn tick_task_sweep(
    state: &mut EventLoopState,
    sender: &GossipSender,
    swarm: &SwarmId,
    author: &Nickname,
    out: &output::Output,
) {
    let now = Instant::now();
    let timeout = Duration::from_secs(task_timeout_secs());
    let expired: Vec<(TaskId, Nickname)> = state
        .tasks
        .iter()
        .filter(|(_, rec)| {
            !rec.state.is_terminal() && now.duration_since(rec.last_activity) > timeout
        })
        .map(|(task_id, rec)| (task_id.clone(), rec.peer.clone()))
        .collect();

    for (task_id, peer) in expired {
        if let Some(rec) = state.tasks.get_mut(&task_id) {
            rec.state = TaskState::Canceled;
        }
        out.task_timeout(&task_id);
        tracing::debug!(%task_id, %peer, "task evicted (idle-debounce timeout)");
        let update = gossip::status_update(
            swarm,
            &task_id,
            TaskState::Canceled,
            None,
            Some(serde_json::json!({ META_REASON: "timeout" })),
        );
        broadcast_status(state, sender, swarm, author, &peer, &task_id, &update).await;
    }

    // GC: drop terminal records past the dedup window so the registry stays
    // bounded over a long-lived daemon-side task churn (the analogue of the
    // heartbeat sweep pruning `quiet_since`). A reaped task's offloaded blobs go
    // with it — unlink their spool files (the review window has long closed).
    let reaped: Vec<TaskId> = state
        .tasks
        .iter()
        .filter(|(_, rec)| {
            rec.state.is_terminal() && now.duration_since(rec.last_activity) > timeout
        })
        .map(|(task_id, _)| task_id.clone())
        .collect();
    if let Some(server) = state.blob_server.as_ref() {
        for task_id in &reaped {
            server.evict_task(task_id).await;
        }
    }
    state.tasks.retain(|_, rec| {
        !rec.state.is_terminal() || now.duration_since(rec.last_activity) <= timeout
    });
}

/// Emit a beat (keepalive) status for every live task whose ball we hold,
/// that we've gone quiet on past the keepalive cadence, and whose **skill**
/// has driven a leg recently — so a silent owner (deciding, executing,
/// reviewing) does not wrongly time out, while a *crashed* skill's task is
/// no longer covered and the peer's debounce reaps it. The task analogue of
/// [`crate::lifecycle::heartbeat::tick_alive`]. See
/// [`TaskRecord::should_keepalive`].
pub(crate) async fn tick_task_keepalive(
    state: &mut EventLoopState,
    sender: &GossipSender,
    swarm: &SwarmId,
    author: &Nickname,
) {
    /// `(task_id, peer, state, last_fraction)` for one keepalive-due task.
    type KeepaliveDue = (TaskId, Nickname, TaskState, Option<(u64, u64)>);

    let now = Instant::now();
    let cadence = Duration::from_secs(task_keepalive_secs());
    let max_silence = Duration::from_secs(task_keepalive_max_secs());
    let due: Vec<KeepaliveDue> = state
        .tasks
        .iter()
        .filter(|(_, rec)| rec.should_keepalive(now, cadence, max_silence))
        .map(|(task_id, rec)| {
            (
                task_id.clone(),
                rec.peer.clone(),
                rec.state,
                rec.last_fraction,
            )
        })
        .collect();

    for (task_id, peer, task_state, fraction) in due {
        let update = gossip::status_update(
            swarm,
            &task_id,
            task_state,
            None,
            Some(gossip::beat_metadata(fraction)),
        );
        broadcast_status(state, sender, swarm, author, &peer, &task_id, &update).await;
        if let Some(rec) = state.tasks.get_mut(&task_id) {
            rec.last_activity = Instant::now();
        }
    }
}

/// Build, sign, and fire-and-forget a daemon-originated status frame (the
/// keepalive beat and the timeout cancel). A serialize error is swallowed
/// like any other plumbing broadcast — the payloads are small literals.
async fn broadcast_status(
    state: &EventLoopState,
    sender: &GossipSender,
    swarm: &SwarmId,
    author: &Nickname,
    peer: &Nickname,
    task_id: &TaskId,
    update: &super::TaskStatusUpdate,
) {
    let Ok(body) = gossip::payload_body(update) else {
        return;
    };
    // Directed status frames are sealed to the peer like every other directed
    // frame (the receive path always unseals a directed body). If the peer's key
    // isn't known yet, skip this beat/cancel — it is fire-and-forget plumbing and
    // a later one retries.
    let Ok(body) = crate::gossip::seal_directed(state, peer, &body) else {
        return;
    };
    let msg = Message::new_a2a_status(swarm, author, peer.clone(), task_id.clone(), body)
        .signed(&state.identity);
    if let Ok(bytes) = msg.serialize() {
        let _ = sender.broadcast(Bytes::from(bytes)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{LegInfo, LegKind, TaskRole, TaskState, apply};
    use crate::a2a::TaskId;
    use crate::protocol::Nickname;
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    fn tid() -> TaskId {
        TaskId::from("550e8400-e29b-41d4-a716-446655440000")
    }

    /// The keepalive gate: I keepalive a task I own the ball on once I've been
    /// quiet past the cadence — but ONLY while a skill has driven a leg within
    /// `max_silence`. Once the skill goes silent past it (a crash), the gate
    /// closes so the daemon stops covering and the peer's debounce reaps it.
    #[test]
    fn keepalive_gated_on_skill_liveness() {
        let cadence = Duration::from_mins(1);
        let max_silence = Duration::from_mins(15);
        let now = Instant::now();

        // A task I own the ball on (offer received ⇒ I'm the Receiver, ball mine).
        let mut tasks = HashMap::new();
        apply(&mut tasks, &leg(LegKind::Offer, false), now);
        let rec = tasks.get_mut(&tid()).unwrap();
        assert!(rec.i_own_ball());

        // Quiet past the cadence and the skill is fresh ⇒ keepalive.
        rec.last_activity = now.checked_sub(2 * cadence).unwrap();
        rec.last_skill_activity = now.checked_sub(cadence).unwrap();
        assert!(rec.should_keepalive(now, cadence, max_silence));

        // Still within the cadence ⇒ not due yet.
        rec.last_activity = now;
        assert!(!rec.should_keepalive(now, cadence, max_silence));

        // Due, but the skill has been silent past `max_silence` (a crash) ⇒
        // the gate closes: the daemon must NOT keep covering the dead task.
        rec.last_activity = now.checked_sub(2 * cadence).unwrap();
        rec.last_skill_activity = now.checked_sub(2 * max_silence).unwrap();
        assert!(!rec.should_keepalive(now, cadence, max_silence));

        // A terminal task is never keepalived.
        rec.last_skill_activity = now;
        rec.state = TaskState::Canceled;
        assert!(!rec.should_keepalive(now, cadence, max_silence));
    }

    fn leg(kind: LegKind, mine: bool) -> LegInfo<'static> {
        // Leak a fixed peer/task_id for 'static convenience in tests.
        LegInfo {
            task_id: Box::leak(Box::new(tid())),
            peer: Box::leak(Box::new(Nickname::from("calm-otter"))),
            kind,
            mine,
            fraction: None,
        }
    }

    /// Drive the happy-path lifecycle from the initiator's view and assert
    /// the A2A state + ball-owner at each step: offer → working (accept) →
    /// input-required (question) → answer → artifact (result, review park) →
    /// completed (confirm).
    #[test]
    fn initiator_happy_path_states_and_ball() {
        let mut tasks = HashMap::new();
        let now = Instant::now();

        apply(&mut tasks, &leg(LegKind::Offer, true), now);
        let rec = &tasks[&tid()];
        assert_eq!(rec.role, TaskRole::Initiator);
        assert_eq!(rec.state, TaskState::Submitted);
        assert_eq!(rec.ball, TaskRole::Receiver);

        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Working), false),
            now,
        );
        assert_eq!(tasks[&tid()].state, TaskState::Working);
        assert_eq!(tasks[&tid()].ball, TaskRole::Receiver);

        // Worker asks a question → input-required, ball flips to me.
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::InputRequired), false),
            now,
        );
        assert_eq!(tasks[&tid()].state, TaskState::InputRequired);
        assert_eq!(tasks[&tid()].ball, TaskRole::Initiator);
        assert!(!tasks[&tid()].review);

        // My answer resumes the work and hands the ball back.
        apply(&mut tasks, &leg(LegKind::Text, true), now);
        assert_eq!(tasks[&tid()].state, TaskState::Working);
        assert_eq!(tasks[&tid()].ball, TaskRole::Receiver);

        // The result parks the task for my review.
        apply(&mut tasks, &leg(LegKind::Artifact, false), now);
        assert_eq!(tasks[&tid()].state, TaskState::InputRequired);
        assert!(tasks[&tid()].review);
        assert_eq!(tasks[&tid()].ball, TaskRole::Initiator);

        // My approval message resumes the worker...
        apply(&mut tasks, &leg(LegKind::Text, true), now);
        assert_eq!(tasks[&tid()].state, TaskState::Working);
        assert_eq!(tasks[&tid()].ball, TaskRole::Receiver);

        // ...and the *worker* authors the terminal `completed` (native A2A).
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Completed), false),
            now,
        );
        assert_eq!(tasks[&tid()].state, TaskState::Completed);
    }

    /// A terminal task is frozen: a later leg never mutates it.
    #[test]
    fn terminal_is_frozen() {
        let mut tasks = HashMap::new();
        let now = Instant::now();
        apply(&mut tasks, &leg(LegKind::Offer, true), now);
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Rejected), false),
            now,
        );
        assert_eq!(tasks[&tid()].state, TaskState::Rejected);
        // A stray confirm after decline must not resurrect it.
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Completed), true),
            now,
        );
        assert_eq!(tasks[&tid()].state, TaskState::Rejected);
    }

    /// A non-offer leg for an unknown task id is dropped (no record created).
    #[test]
    fn unknown_task_non_offer_is_ignored() {
        let mut tasks = HashMap::new();
        apply(&mut tasks, &leg(LegKind::Text, false), Instant::now());
        assert!(tasks.is_empty());
    }

    /// Out-of-order legs (a confirm before any review park) are ignored.
    #[test]
    fn out_of_order_is_ignored() {
        let mut tasks = HashMap::new();
        let now = Instant::now();
        apply(&mut tasks, &leg(LegKind::Offer, true), now);
        // A confirm straight after the offer — no review to close.
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Completed), true),
            now,
        );
        assert_eq!(tasks[&tid()].state, TaskState::Submitted);
    }

    /// Direction guards (native A2A — the worker drives its own task):
    /// an *initiator*-authored `working` (accepting is the worker's) and an
    /// *initiator*-authored `completed` (the worker closes the task) are both
    /// ignored; the worker's own `completed` is what closes it.
    #[test]
    fn misdirected_statuses_are_ignored() {
        let mut tasks = HashMap::new();
        let now = Instant::now();
        apply(&mut tasks, &leg(LegKind::Offer, true), now);
        // Initiator cannot accept its own offer.
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Working), true),
            now,
        );
        assert_eq!(tasks[&tid()].state, TaskState::Submitted);
        // Worker accepts, returns a result → review park.
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Working), false),
            now,
        );
        apply(&mut tasks, &leg(LegKind::Artifact, false), now);
        // The initiator cannot author `completed` — only the worker closes it.
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Completed), true),
            now,
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::InputRequired,
            "an initiator-authored completed is ignored"
        );
        // The worker's own `completed` closes it.
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Completed), false),
            now,
        );
        assert_eq!(tasks[&tid()].state, TaskState::Completed);
    }

    /// A revision loop: artifact → review park → the initiator's `change`
    /// message resumes the work → a second artifact parks again.
    #[test]
    fn change_loops_back_to_working() {
        let mut tasks = HashMap::new();
        let now = Instant::now();
        apply(&mut tasks, &leg(LegKind::Offer, true), now);
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Working), false),
            now,
        );
        apply(&mut tasks, &leg(LegKind::Artifact, false), now);
        assert!(tasks[&tid()].review);
        apply(&mut tasks, &leg(LegKind::Text, true), now); // change verdict
        assert_eq!(tasks[&tid()].state, TaskState::Working);
        assert!(!tasks[&tid()].review);
        apply(&mut tasks, &leg(LegKind::Artifact, false), now);
        assert_eq!(tasks[&tid()].state, TaskState::InputRequired);
        assert!(tasks[&tid()].review);
    }

    /// Content legs count toward the cap; beats do not, and the cap
    /// crossing is reported exactly once.
    #[test]
    fn content_cap_excludes_beats() {
        let mut tasks = HashMap::new();
        let now = Instant::now();
        apply(&mut tasks, &leg(LegKind::Offer, true), now); // count 1
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Working), false),
            now,
        ); // count 2
        for _ in 0..50 {
            // Beats never increment the content count.
            apply(&mut tasks, &leg(LegKind::Beat, false), now);
        }
        assert_eq!(tasks[&tid()].content_count, 2);
        // Drive to exactly the cap, then one past it.
        let mut over = false;
        while tasks[&tid()].content_count < super::TASK_CONTENT_CAP {
            over |= apply(&mut tasks, &leg(LegKind::Text, false), now);
        }
        assert!(!over, "no crossing while at/under the cap");
        let crossing = apply(&mut tasks, &leg(LegKind::Text, false), now);
        assert!(crossing, "the leg that pushes past the cap reports it");
    }
}
