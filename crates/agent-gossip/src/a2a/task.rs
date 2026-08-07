//! The daemon-side **task** state machine + its two timers, over the A2A
//! lifecycle.
//!
//! A task is a directed, multi-leg conversation whose wire form is pure A2A:
//! a `message/send` (no `taskId`) opens it — the worker mints the id and
//! returns the `Task` — then `TaskStatusUpdate`s and mid-task `message/send`s
//! advance it, a `TaskArtifactUpdate` returns the result, and the **worker's**
//! `completed` status closes it (after the initiator's approval message). The
//! daemon owns only the *coarse* lifecycle — state advance, the per-task idle
//! debounce, and the ball-owner keepalive — while the skill owns the
//! *content*.
//!
//! The machine is **distributed** with no consensus: each party derives
//! state from the legs it has seen, so the rules are deliberately
//! conservative — **monotonic** advance (a leg that would move backward is
//! ignored), **idempotent** (a duplicate leg is a no-op), and a terminal
//! record is frozen. Local triggers (the timeout) *broadcast* a terminal
//! `canceled` status so the other side converges.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bytes::Bytes;

use crate::a2a::app::A2aApp;
use crate::a2a::tuning::{task_keepalive_max_secs, task_keepalive_secs, task_timeout_secs};
use crate::output;
use fofoca::embed::EventLoopState;
use fofoca::embed::HandlerCtx;
use fofoca::protocol::{Message, MessageKind, Nickname};

use super::{META_REASON, TaskId, TaskState, gossip, wire};

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

/// The value cluster [`ingest`] needs beyond its `tasks` registry handle.
#[derive(Clone, Copy)]
pub(crate) struct IngestLegParams<'a> {
    pub(crate) frame: &'a Message,
    pub(crate) task_id: &'a TaskId,
    pub(crate) mine: bool,
    pub(crate) now: Instant,
}

/// Feed one **validated logical** task frame into the registry from the
/// broadcast (`mine = true`) or receive (`mine = false`) path. The single
/// call site for both: it reduces the frame to a [`LegInfo`] (deriving the
/// peer and the beat fraction) and advances the machine. A frame that
/// belongs to no task is a no-op.
pub(crate) fn ingest(tasks: &mut HashMap<TaskId, TaskRecord>, params: IngestLegParams<'_>) {
    let IngestLegParams {
        frame,
        task_id,
        mine,
        now,
    } = params;
    // A task leg is a directed status/artifact app frame. `a2a_msg` (broadcast
    // chat) and every infra kind are never task legs; task `message/send` legs
    // are applied directly from the RPC path, not through `ingest`. The caller
    // already holds the `task_id` (8.0 moved it into the body), so it is threaded
    // in rather than recovered from the body.
    //
    // On the `mine` path the caller must hand us the plaintext twin of the leg,
    // not the wire frame: the wire body is sealed to the addressee, and a status
    // leg reads its state out of the body.
    let MessageKind::App {
        tag, to: Some(to), ..
    } = &frame.kind
    else {
        return;
    };
    let (kind, fraction) = match tag.as_str() {
        wire::STATUS => {
            let Ok(payload) = gossip::status_payload(frame) else {
                return;
            };
            if gossip::is_beat(&payload) {
                (LegKind::Beat, gossip::beat_fraction(&payload))
            } else {
                (LegKind::Status(payload.status.state), None)
            }
        }
        wire::ARTIFACT => (LegKind::Artifact, None),
        _ => return,
    };
    let peer = if mine { to } else { &frame.author };
    apply(
        tasks,
        &LegInfo {
            task_id,
            peer,
            kind,
            mine,
            fraction,
        },
        now,
    );
}

/// The value cluster [`adopt_initiator`] needs beyond its `tasks` registry
/// handle.
#[derive(Clone, Copy)]
pub(crate) struct AdoptInitiatorParams<'a> {
    pub(crate) task_id: &'a TaskId,
    pub(crate) peer: &'a Nickname,
    pub(crate) task_state: TaskState,
    pub(crate) now: Instant,
}

/// Upsert the **initiator**-side record for a task whose authoritative `Task`
/// the worker just returned over RPC (`message/send` create/follow-up). The
/// worker (server) is the source of truth for the task's state, so we mirror
/// `task_state` rather than replay legs. Terminal records are frozen.
pub(crate) fn adopt_initiator(
    tasks: &mut HashMap<TaskId, TaskRecord>,
    params: AdoptInitiatorParams<'_>,
) {
    let AdoptInitiatorParams {
        task_id,
        peer,
        task_state,
        now,
    } = params;
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
            // …and only from the task's own worker. The caller has already
            // proved this response answers a call we made to `peer`, but that
            // says nothing about *which* task the answer may name: any peer we
            // call once could otherwise return a Task id belonging to a
            // different peer and drive it terminal, or keep refreshing it past
            // the reaper. The record's own counterparty is the authority.
            if rec.peer != *peer {
                return;
            }
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
/// Monotonic + idempotent + terminal-frozen.
pub(crate) fn apply(tasks: &mut HashMap<TaskId, TaskRecord>, leg: &LegInfo<'_>, now: Instant) {
    // A terminal record is immutable — late/duplicate legs are ignored.
    if tasks
        .get(leg.task_id)
        .is_some_and(|rec| rec.state.is_terminal())
    {
        return;
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
                last_fraction: None,
                ball: TaskRole::Receiver,
            });
    }

    let Some(rec) = tasks.get_mut(leg.task_id) else {
        // A non-offer leg for a task we never saw open — drop it (out of
        // order, or a task that began before our join horizon).
        return;
    };

    // Only the task's own counterparty may drive it. The frame is
    // signature-verified upstream, but a signature is not a party: `advance`
    // *derives* the sender from our own role, so without this an unchecked leg
    // is attributed to the counterparty by construction and any mesh member
    // could close, park, or beat a task they are not in. The RPC leg path makes
    // the same check by hand (`node.rs`'s `ingest_remote_message`); this is the
    // push plane's, where nearly every transition happens.
    if rec.peer != *leg.peer {
        return;
    }

    advance(rec, leg.kind, leg.mine);
    if let Some(fraction) = leg.fraction {
        rec.last_fraction = Some(fraction);
    }
    rec.last_activity = now;
    // A real leg (skill-sent or peer-received) proves a skill is driving this
    // task; the daemon's own keepalive never routes through `apply`, so it
    // cannot refresh this clock and thus cannot cover for a dead skill forever.
    rec.last_skill_activity = now;
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
            // `completed`, after the initiator's approval message.
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
/// analogue of the engine's own lifecycle silence sweep.
pub(crate) async fn tick_task_sweep(
    state: &mut EventLoopState,
    app: &mut A2aApp,
    ctx: &HandlerCtx<'_>,
    out: &output::Output,
) {
    let now = Instant::now();
    let timeout = Duration::from_secs(task_timeout_secs());
    let (expired, reaped) = sweep_registry(&mut app.tasks, now, timeout);

    for (task_id, peer) in expired {
        out.task_timeout(&task_id, output::TaskGoneReason::Timeout);
        tracing::debug!(%task_id, %peer, "task evicted (idle-debounce timeout)");
        let update = gossip::status_update(
            ctx.mesh,
            gossip::StatusUpdateParams {
                task_id: &task_id,
                state: TaskState::Canceled,
                note: None,
                metadata: Some(serde_json::json!({ META_REASON: "timeout" })),
            },
        );
        broadcast_status(
            state,
            ctx,
            BroadcastStatusParams {
                peer: &peer,
                task_id: &task_id,
                update: &update,
            },
        )
        .await;
    }

    // A reaped task's offloaded blobs go with it — unlink their spool files
    // (the review window has long closed).
    if let Some(server) = app.blob_server.as_ref() {
        for task_id in &reaped {
            server
                .evict_content(&fofoca::ops::blob::ContentId::new(task_id.as_str()))
                .await;
        }
    }
}

/// The registry half of [`tick_task_sweep`]: cancel what has gone idle, then
/// drop terminal records past the dedup window. Returns the cancelled tasks
/// (with the peer each must be told about) and the ids that were reaped.
///
/// Split out from the IO so the ordering rule it encodes is testable: a record
/// cancelled *in this pass* must survive it. Both passes read one `now`, and
/// the GC's own justification — a terminal record older than the timeout is
/// past the dedup window, so no further leg for it can arrive — is only true
/// for a record that has actually *been* terminal that long. Cancelling
/// restamps `last_activity`, which is what starts that clock; without it the
/// GC matched every task the expiry pass had just cancelled, and the freeze
/// window was zero. A task could then reach `canceled`, fire `task_timeout`,
/// and be minted live again by the next `GetTask` — while the worker's own
/// `a2a artifact` for it failed as an unknown task instead of surfacing the
/// cancel.
fn sweep_registry(
    tasks: &mut HashMap<TaskId, TaskRecord>,
    now: Instant,
    timeout: Duration,
) -> (Vec<(TaskId, Nickname)>, Vec<TaskId>) {
    let mut expired = Vec::new();
    for (task_id, rec) in tasks.iter_mut() {
        if !rec.state.is_terminal() && now.duration_since(rec.last_activity) > timeout {
            rec.state = TaskState::Canceled;
            rec.last_activity = now;
            expired.push((task_id.clone(), rec.peer.clone()));
        }
    }

    // GC: drop terminal records past the dedup window so the registry stays
    // bounded over a long-lived daemon-side task churn (the analogue of the
    // heartbeat sweep pruning `quiet_since`).
    let reaped: Vec<TaskId> = tasks
        .iter()
        .filter(|(_, rec)| {
            rec.state.is_terminal() && now.duration_since(rec.last_activity) > timeout
        })
        .map(|(task_id, _)| task_id.clone())
        .collect();
    tasks.retain(|_, rec| {
        !rec.state.is_terminal() || now.duration_since(rec.last_activity) <= timeout
    });
    (expired, reaped)
}

/// Terminally cancel every non-terminal task whose counterparty broadcast a
/// graceful `Left` — its next leg can never arrive, so waiting out the idle
/// debounce only delays the inevitable. No wire broadcast, unlike the sweep's
/// eviction: task records are strictly per-pair (third-party relays never
/// insert), and the only other holder just left. Freezing the record also
/// stops [`tick_task_keepalive`] (`should_keepalive` gates on non-terminal),
/// and the frozen record ages into [`tick_task_sweep`]'s GC as usual.
pub(crate) fn fail_tasks_for_departed_peer(
    tasks: &mut HashMap<TaskId, TaskRecord>,
    peer: &Nickname,
    out: &output::Output,
) {
    let now = Instant::now();
    for (task_id, rec) in tasks.iter_mut() {
        if rec.peer == *peer && !rec.state.is_terminal() {
            rec.state = TaskState::Canceled;
            // Start the terminal clock, so "ages into the GC" is true here as
            // well: a task already idle when its peer left would otherwise be
            // cancelled and reaped by the very next sweep, with no window in
            // which a late leg is recognized as a duplicate rather than an
            // unknown task.
            rec.last_activity = now;
            out.task_timeout(task_id, output::TaskGoneReason::PeerLeft);
            tracing::debug!(%task_id, %peer, "task canceled (peer left)");
        }
    }
}

/// Emit a beat (keepalive) status for every live task whose ball we hold,
/// that we've gone quiet on past the keepalive cadence, and whose **skill**
/// has driven a leg recently — so a silent owner (deciding, executing,
/// reviewing) does not wrongly time out, while a *crashed* skill's task is
/// no longer covered and the peer's debounce reaps it. The task analogue of
/// the engine's own lifecycle keepalive tick. See
/// [`TaskRecord::should_keepalive`].
pub(crate) async fn tick_task_keepalive(
    state: &mut EventLoopState,
    app: &mut A2aApp,
    ctx: &HandlerCtx<'_>,
) {
    /// `(task_id, peer, state, last_fraction)` for one keepalive-due task.
    type KeepaliveDue = (TaskId, Nickname, TaskState, Option<(u64, u64)>);

    let now = Instant::now();
    let cadence = Duration::from_secs(task_keepalive_secs());
    let max_silence = Duration::from_secs(task_keepalive_max_secs());
    let due: Vec<KeepaliveDue> = app
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
            ctx.mesh,
            gossip::StatusUpdateParams {
                task_id: &task_id,
                state: task_state,
                note: None,
                metadata: Some(gossip::beat_metadata(fraction)),
            },
        );
        broadcast_status(
            state,
            ctx,
            BroadcastStatusParams {
                peer: &peer,
                task_id: &task_id,
                update: &update,
            },
        )
        .await;
        if let Some(rec) = app.tasks.get_mut(&task_id) {
            rec.last_activity = Instant::now();
        }
    }
}

/// The value cluster [`broadcast_status`] needs beyond its `state`/`ctx`
/// handles.
struct BroadcastStatusParams<'a> {
    peer: &'a Nickname,
    /// The task id rides inside the status payload body (8.0); the envelope no
    /// longer carries it, so this is unused here beyond documenting the leg.
    task_id: &'a TaskId,
    update: &'a super::TaskStatusUpdate,
}

/// Build, sign, and fire-and-forget a daemon-originated status frame (the
/// keepalive beat and the timeout cancel). A serialize error is swallowed
/// like any other plumbing broadcast — the payloads are small literals.
///
/// **Do not fold this into `send::broadcast_directed_frame`.** It looks like a
/// duplicate of it and it is not: that path self-ingests through
/// `send::ingest_own_leg`, and a beat routed through `apply` would refresh
/// `last_skill_activity` (see [`TaskRecord::should_keepalive`]) — the one clock
/// that stops a *crashed* skill's daemon from keepaliving its tasks forever.
/// Unify the two and every abandoned task becomes immortal: beaten indefinitely,
/// never reaped. The keepalive hand-sets `last_activity` above instead, which is
/// exactly the point. `monitor_contract`'s
/// `test_task_times_out_when_skill_goes_silent` is what catches this.
async fn broadcast_status(
    state: &EventLoopState,
    ctx: &HandlerCtx<'_>,
    params: BroadcastStatusParams<'_>,
) {
    let BroadcastStatusParams {
        peer,
        task_id: _task_id,
        update,
    } = params;
    let Ok(body) = gossip::payload_body(update) else {
        return;
    };
    // Directed status frames are sealed to the peer like every other directed
    // frame (the receive path always unseals a directed body). If the peer's key
    // isn't known yet, skip this beat/cancel — it is fire-and-forget plumbing and
    // a later one retries.
    let Ok(body) = crate::a2a::send::seal_directed(state, peer, &body) else {
        return;
    };
    let kind = MessageKind::App {
        tag: fofoca::protocol::AppTag::from(wire::STATUS),
        to: Some(peer.clone()),
        corr: None,
    };
    let msg = Message::new_frame(ctx.mesh, ctx.author, kind, body).signed(state.identity());
    if let Ok(bytes) = msg.serialize() {
        // Through `deliver`, not `sender.broadcast`: the frame is directed, so
        // it takes unicast to `peer` like every other directed frame. Straight
        // onto gossip it would flood `author`/`to` in the clear on a timer, and
        // every bystander would run `lifecycle::observe` on a beat meant for
        // one peer — waking a parked bell off someone else's task.
        let _ = fofoca::ops::deliver(&msg, Bytes::from(bytes), state, ctx.sender).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{LegInfo, LegKind, TaskRecord, TaskRole, TaskState, apply};
    use crate::a2a::TaskId;
    use fofoca::protocol::Nickname;
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

    /// A leg from a peer who is not the task's counterparty is dropped, whatever
    /// it claims. Pre-fix `apply` looked the record up by task id alone and
    /// `advance` derived the sender from our own role, so a bystander's status
    /// frame was applied *as the counterparty's*: `completed` froze the record
    /// (dropping the real worker's artifact), `artifact` parked it in review with
    /// the bystander's text, and a `Beat` refreshed `last_skill_activity` — the
    /// one clock that must stay unreachable from plumbing, or an abandoned task
    /// never gets reaped.
    #[test]
    fn a_leg_from_a_non_party_is_dropped() {
        let now = Instant::now();
        let outsider = Box::leak(Box::new(Nickname::from("wire-thistle")));
        let from_outsider = |kind: LegKind| LegInfo {
            task_id: Box::leak(Box::new(tid())),
            peer: outsider,
            kind,
            mine: false,
            fraction: None,
        };

        // We are the *initiator* — the side the attack lands on, since an
        // inbound leg is credited to the worker — of a task `calm-otter`
        // accepted.
        let mut tasks = HashMap::new();
        apply(&mut tasks, &leg(LegKind::Offer, true), now);
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Working), false),
            now,
        );
        assert_eq!(tasks[&tid()].state, TaskState::Working);
        let skill_clock = tasks[&tid()].last_skill_activity;

        apply(
            &mut tasks,
            &from_outsider(LegKind::Status(TaskState::Completed)),
            now,
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Working,
            "a non-party cannot close the task"
        );

        apply(
            &mut tasks,
            &from_outsider(LegKind::Status(TaskState::Canceled)),
            now,
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Working,
            "a non-party cannot cancel the task"
        );

        apply(&mut tasks, &from_outsider(LegKind::Artifact), now);
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Working,
            "a non-party cannot park the task in review"
        );
        assert!(!tasks[&tid()].review);

        let later = now.checked_add(Duration::from_mins(30)).unwrap();
        apply(&mut tasks, &from_outsider(LegKind::Beat), later);
        assert_eq!(
            tasks[&tid()].last_skill_activity, skill_clock,
            "a non-party's beat cannot refresh the skill-liveness clock"
        );

        // The real counterparty still drives it.
        apply(&mut tasks, &leg(LegKind::Artifact, false), now);
        assert_eq!(tasks[&tid()].state, TaskState::InputRequired);
        assert!(tasks[&tid()].review);
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Completed), false),
            now,
        );
        assert_eq!(tasks[&tid()].state, TaskState::Completed);
    }

    /// A task the sweep cancels survives that same sweep.
    ///
    /// Both passes read one `now`, and the expiry pass used to set `Canceled`
    /// without touching `last_activity` — so the GC's filter (terminal, and
    /// idle past the timeout) matched every record it had just cancelled, by
    /// construction. The freeze window the GC justifies itself with was zero.
    /// Pre-fix this test finds an empty registry: the task fired `task_timeout`
    /// and then vanished, so the next `GetTask` would mint it live again and
    /// the worker's own `a2a artifact` for it would fail as an unknown task.
    #[test]
    fn a_task_cancelled_by_the_sweep_is_not_reaped_in_the_same_pass() {
        let timeout = Duration::from_mins(2);
        let now = Instant::now();
        let mut tasks = HashMap::new();
        apply(&mut tasks, &leg(LegKind::Offer, false), now);
        // Idle well past the debounce.
        tasks.get_mut(&tid()).unwrap().last_activity =
            now.checked_sub(timeout * 2).expect("test clock");

        let (expired, reaped) = super::sweep_registry(&mut tasks, now, timeout);

        assert_eq!(expired.len(), 1, "the idle task is cancelled");
        assert!(reaped.is_empty(), "and is not reaped in the same pass");
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Canceled,
            "the record is retained, frozen terminal, for the dedup window"
        );

        // It reaps once it has actually been terminal for the window.
        let later = now.checked_add(timeout * 2).expect("test clock");
        let (still_live, now_reaped) = super::sweep_registry(&mut tasks, later, timeout);
        assert!(
            still_live.is_empty(),
            "a terminal record is not re-cancelled"
        );
        assert_eq!(now_reaped, vec![tid()]);
        assert!(tasks.is_empty(), "and the registry stays bounded");
    }

    /// An RPC `Task` snapshot is adopted only from the task's own worker.
    ///
    /// The receive path already proves the response answers a call we made to
    /// that peer, but a call to *anyone* was enough: pre-fix, one reply naming
    /// another peer's task id drove it terminal (`rec.peer` still read the
    /// honest worker, so nothing recorded who killed it), and a non-terminal
    /// snapshot refreshed `last_activity` unconditionally, which held any of
    /// our tasks off the reaper for as long as the caller kept answering.
    #[test]
    fn a_returned_task_is_adopted_only_from_its_own_worker() {
        let now = Instant::now();
        let mut tasks = HashMap::new();
        apply(&mut tasks, &leg(LegKind::Offer, true), now);
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Working), false),
            now,
        );
        let before = tasks[&tid()].last_activity;

        let adopt = |registry: &mut HashMap<TaskId, TaskRecord>, peer: &str, state, when| {
            super::adopt_initiator(
                registry,
                super::AdoptInitiatorParams {
                    task_id: &tid(),
                    peer: &Nickname::from(peer),
                    task_state: state,
                    now: when,
                },
            );
        };

        let later = now.checked_add(Duration::from_mins(5)).unwrap();
        adopt(&mut tasks, "wire-thistle", TaskState::Failed, later);
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Working,
            "another peer's answer cannot drive our task terminal"
        );
        assert_eq!(
            tasks[&tid()].last_activity, before,
            "nor hold it off the reaper by refreshing its activity clock"
        );

        // The real worker's snapshot is still adopted.
        adopt(&mut tasks, "calm-otter", TaskState::Failed, later);
        assert_eq!(tasks[&tid()].state, TaskState::Failed);
    }

    /// A second `Offer` on a live task id from a different peer must not
    /// re-point the record: the id is the attacker's only handle, and the offer
    /// arm is the one place `leg.peer` was ever read.
    #[test]
    fn a_foreign_offer_does_not_rebind_a_live_task() {
        let now = Instant::now();
        let mut tasks = HashMap::new();
        apply(&mut tasks, &leg(LegKind::Offer, false), now);

        apply(
            &mut tasks,
            &LegInfo {
                task_id: Box::leak(Box::new(tid())),
                peer: Box::leak(Box::new(Nickname::from("wire-thistle"))),
                kind: LegKind::Offer,
                mine: false,
                fraction: None,
            },
            now,
        );

        assert_eq!(
            tasks[&tid()].peer,
            Nickname::from("calm-otter"),
            "the counterparty is fixed at record creation"
        );
    }

    /// The graceful-`Left` cancel pass hits exactly the departed peer's live
    /// tasks: another peer's task and an already-terminal record with the
    /// departed peer are both untouched.
    #[test]
    fn peer_left_cancels_only_that_peers_live_tasks() {
        let now = Instant::now();
        let departed = Nickname::from("calm-otter");
        let offer = |task_id: &'static str, peer: &'static str| LegInfo {
            task_id: Box::leak(Box::new(TaskId::from(task_id))),
            peer: Box::leak(Box::new(Nickname::from(peer))),
            kind: LegKind::Offer,
            mine: false,
            fraction: None,
        };
        let live_id = "550e8400-e29b-41d4-a716-446655440001";
        let staying_id = "550e8400-e29b-41d4-a716-446655440002";
        let done_id = "550e8400-e29b-41d4-a716-446655440003";

        let mut tasks = HashMap::new();
        apply(&mut tasks, &offer(live_id, "calm-otter"), now);
        apply(&mut tasks, &offer(staying_id, "drift-oak"), now);
        apply(&mut tasks, &offer(done_id, "calm-otter"), now);
        tasks.get_mut(&TaskId::from(done_id)).unwrap().state = TaskState::Completed;

        let out = crate::output::Output::silent();
        super::fail_tasks_for_departed_peer(&mut tasks, &departed, &out);

        assert_eq!(
            tasks[&TaskId::from(live_id)].state,
            TaskState::Canceled,
            "the departed peer's live task is canceled"
        );
        assert!(
            !tasks[&TaskId::from(staying_id)].state.is_terminal(),
            "another peer's task is untouched"
        );
        assert_eq!(
            tasks[&TaskId::from(done_id)].state,
            TaskState::Completed,
            "a terminal record is frozen, not rewritten to canceled"
        );
    }

    /// Regression for findings ① (daemon panic) + ② (state divergence): a
    /// worker's OWN echoed artifact leg carries a **sealed** body on a
    /// passworded mesh, so nothing may recover the task id by re-parsing it.
    /// Threading the id into `ingest` means the self-echo still advances the
    /// record to `input-required` (not stuck `working`); and the app surfaces
    /// the *plaintext* logical frame (never the sealed wire body), so its JSON
    /// render never panics on a missing task id.
    ///
    /// Pre-fix, `task::ingest` re-parsed the sealed body for the id → `None` →
    /// returned before `apply`, leaving the task `working`; and the JSON sink's
    /// `frame_task_id(msg).expect(...)` would panic on a body that (like
    /// ciphertext) is not a valid payload.
    #[test]
    fn own_sealed_artifact_echo_advances_and_renders_without_panic() {
        use fofoca::protocol::{AppFrameParams, AppTag, MeshId, Message, MessageBody};

        let mut tasks = HashMap::new();
        let now = Instant::now();
        let task_id = tid();
        let mesh = MeshId::from("test");

        // Worker receives the offer (⇒ Receiver) and commits to `working`.
        apply(&mut tasks, &leg(LegKind::Offer, false), now);
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Working), true),
            now,
        );
        assert_eq!(tasks[&tid()].state, TaskState::Working);

        // The worker's own echoed artifact leg: directed, body is a sealed
        // ciphertext stand-in that does not parse as a `TaskArtifactUpdate`.
        let sealed = Message::new_app(
            &mesh,
            &Nickname::from("worker-bot"),
            AppFrameParams {
                tag: AppTag::from(crate::a2a::wire::ARTIFACT),
                to: Some(Nickname::from("calm-otter")),
                corr: None,
                body: MessageBody::new("sealed-ciphertext-stand-in").expect("valid body"),
            },
        );
        assert!(
            crate::a2a::gossip::frame_task_id(&sealed).is_none(),
            "a sealed body yields no parseable task id — the condition under test"
        );

        // ②: the self-echo still advances the record (id threaded, not parsed).
        super::ingest(
            &mut tasks,
            super::IngestLegParams {
                frame: &sealed,
                task_id: &task_id,
                mine: true,
                now,
            },
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::InputRequired,
            "the worker's own artifact echo must park the task for review"
        );
        assert!(tasks[&tid()].review);

        // ①: the app surfaces the *plaintext* logical frame it built (the same
        // `task_id`, unsealed), so rendering that self-echo as JSON must not
        // panic and must carry the id.
        let payload = crate::a2a::gossip::artifact_update(&mesh, &task_id, "result text");
        let plaintext = Message::new_app(
            &mesh,
            &Nickname::from("worker-bot"),
            AppFrameParams {
                tag: AppTag::from(crate::a2a::wire::ARTIFACT),
                to: Some(Nickname::from("calm-otter")),
                corr: None,
                body: MessageBody::new(
                    serde_json::to_string(&payload).expect("payload serializes"),
                )
                .expect("valid body"),
            },
        );
        let line = crate::output::event_json(&crate::output::OutputEvent::Task {
            msg: Box::new(plaintext),
            is_self: true,
        })
        .expect("a task event renders a JSON line");
        let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid JSON");
        assert_eq!(parsed["task_id"], task_id.as_str());
        assert_eq!(parsed["self"], true);
    }

    /// The **ingest contract** for a worker's own status leg: a sealed body is a
    /// no-op, a plaintext one advances the record. Only STATUS legs read their
    /// state out of the body, which is why only they were affected — the artifact
    /// leg needs nothing from it and always self-echoed correctly.
    ///
    /// Pre-fix, `broadcast_directed_frame` handed `ingest_own_leg` the **sealed** wire
    /// frame, so the parse failed, `ingest` returned before `apply`, and a
    /// worker's own `completed` never reached its own record. The worker is the
    /// A2A *server*, so `GetTask` — served from that record — kept answering
    /// `working` for an approved task, and `tick_task_sweep` later reaped it,
    /// firing `task_timeout` and broadcasting a `canceled` for finished work. The
    /// idle clock it reaped against sat at the last leg that *did* reach `apply`
    /// (the artifact); with no artifact, the keepalive covered the task until
    /// `TASK_KEEPALIVE_MAX_SECS` ran out and the timeout ran from there.
    ///
    /// This pins `ingest`, **not** its callers: it calls `ingest` directly. The
    /// caller contract — `broadcast_directed_frame` passing the plaintext twin it
    /// already builds — is pinned at integration level, where a caller regression
    /// is actually observable.
    #[test]
    fn own_status_echo_completes_the_workers_own_record() {
        use fofoca::protocol::{AppFrameParams, AppTag, MeshId, Message, MessageBody};

        let mesh = MeshId::from("test");
        let task_id = tid();
        let now = Instant::now();

        // The worker's own status leg, as the plaintext twin (`sealed: false`)
        // or as the wire frame the addressee alone can read (`sealed: true`).
        let status_frame = |state: TaskState, sealed: bool| {
            let body = if sealed {
                "sealed-ciphertext-stand-in".to_owned()
            } else {
                let payload = crate::a2a::gossip::status_update(
                    &mesh,
                    crate::a2a::gossip::StatusUpdateParams {
                        task_id: &task_id,
                        state,
                        note: None,
                        metadata: None,
                    },
                );
                serde_json::to_string(&payload).expect("payload serializes")
            };
            Message::new_app(
                &mesh,
                &Nickname::from("worker-bot"),
                AppFrameParams {
                    tag: AppTag::from(crate::a2a::wire::STATUS),
                    to: Some(Nickname::from("calm-otter")),
                    corr: None,
                    body: MessageBody::new(&body).expect("valid body"),
                },
            )
        };
        let ingest_own = |tasks: &mut HashMap<_, _>, frame: &Message| {
            super::ingest(
                tasks,
                super::IngestLegParams {
                    frame,
                    task_id: &task_id,
                    mine: true,
                    now,
                },
            );
        };

        // Worker receives the offer (⇒ Receiver) and drives itself to `working`.
        let mut tasks = HashMap::new();
        apply(&mut tasks, &leg(LegKind::Offer, false), now);
        ingest_own(&mut tasks, &status_frame(TaskState::Working, false));
        assert_eq!(tasks[&tid()].state, TaskState::Working);

        // The wire frame is unreadable to its own sender: ingesting *that* is
        // the bug, and it must stay a no-op rather than silently half-advance.
        ingest_own(&mut tasks, &status_frame(TaskState::Completed, true));
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Working,
            "a sealed body carries no state — the caller must ingest the plaintext twin"
        );

        // The plaintext twin closes the worker's own record.
        ingest_own(&mut tasks, &status_frame(TaskState::Completed, false));
        assert_eq!(tasks[&tid()].state, TaskState::Completed);
        assert!(tasks[&tid()].state.is_terminal());

        // And so the idle sweep passes it over, however long it sits there.
        // This is `tick_task_sweep`'s expiry predicate, verbatim.
        let timeout = Duration::from_secs(crate::a2a::task::task_timeout_secs());
        let rec = tasks.get_mut(&tid()).expect("the record is live");
        rec.last_activity = now.checked_sub(10 * timeout).expect("in range");
        let expired = !rec.state.is_terminal() && now.duration_since(rec.last_activity) > timeout;
        assert!(
            !expired,
            "a completed task is never swept, so it never emits a spurious task_timeout"
        );
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
}
