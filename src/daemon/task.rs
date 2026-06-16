//! The daemon-side **task** state machine + its two timers.
//!
//! A task is a directed, multi-leg exchange (see [`crate::protocol::MessageKind::Task`]);
//! `handover` is one behavior on it. The daemon owns only the *coarse*
//! lifecycle — phase advance, the per-task idle debounce, the ball-owner
//! keepalive, and the content-message cap — while the skill owns the
//! *content* (what to ask, whether to confirm, the brief, the plan).
//!
//! The machine is **distributed** with no consensus: each party derives
//! state from the legs it has seen, so the rules are deliberately
//! conservative — **monotonic** advance (a leg that would move backward is
//! ignored), **idempotent** (a duplicate leg is a no-op), and a terminal
//! record is frozen. Local triggers (timeout, cap) *broadcast* a terminal
//! `Cancel` so the other side converges.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use iroh_gossip::api::GossipSender;

use crate::daemon::state::EventLoopState;
use crate::output;
use crate::protocol::{
    Message, MessageBody, MessageKind, Nickname, SwarmId, TaskId, TaskKind, TaskPhase,
};
use crate::util::consts::TASK_CONTENT_CAP;
use crate::util::tuning::{task_keepalive_secs, task_timeout_secs};

/// My part in a task: did I open it, or receive the offer?
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

/// The coarse lifecycle the daemon tracks. Ordered so a backward
/// transition is a cheap comparison: `Proposed < Active < Review < Terminal`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum TaskState {
    Proposed,
    Active,
    Review,
    Terminal,
}

/// One in-flight task this node is a party to.
#[derive(Clone, Debug)]
pub(crate) struct TaskRecord {
    /// The other party.
    pub peer: Nickname,
    pub kind: TaskKind,
    pub role: TaskRole,
    pub state: TaskState,
    /// Local-clock instant of the last leg (inbound or our own) — the
    /// debounce reads this, never the wire `ts` (which can skew).
    pub last_activity: Instant,
    /// Count of **content** legs seen (progress excluded) — the cap.
    pub content_count: u32,
    /// Last progress fraction the receiver reported, replayed on keepalives.
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
}

/// The fields of an applied leg the state machine needs.
pub(crate) struct LegInfo<'a> {
    pub task_id: &'a TaskId,
    /// The other party (inbound: the author; outbound: the `to`).
    pub peer: &'a Nickname,
    pub kind: TaskKind,
    pub phase: TaskPhase,
    /// `true` if we sent this leg (outbound echo), `false` if it arrived.
    pub mine: bool,
    /// Parsed `done/total` for a `Progress` leg (else `None`).
    pub fraction: Option<(u64, u64)>,
}

/// Parse a `Progress` body's `done/total` fraction (e.g. `"35/100"`);
/// `None` for an empty or unparseable beat (indeterminate progress).
pub(crate) fn parse_fraction(body: &str) -> Option<(u64, u64)> {
    let (done, total) = body.split_once('/')?;
    Some((done.trim().parse().ok()?, total.trim().parse().ok()?))
}

/// Feed a `Task` message into the registry from the broadcast (`mine =
/// true`) or receive (`mine = false`) path. The single call site for both:
/// it derives the peer (the `to` we sent to, or the `author` we heard from)
/// and the progress fraction, then advances the machine. A non-`Task`
/// message is a no-op. Returns `true` if this leg crossed the content cap.
pub(crate) fn observe(
    tasks: &mut HashMap<TaskId, TaskRecord>,
    msg: &Message,
    mine: bool,
    now: Instant,
) -> bool {
    let MessageKind::Task {
        to,
        task_id,
        kind,
        phase,
    } = &msg.kind
    else {
        return false;
    };
    let peer = if mine { to } else { &msg.author };
    let fraction = matches!(phase, TaskPhase::Progress)
        .then(|| parse_fraction(msg.body.as_str()))
        .flatten();
    apply(
        tasks,
        &LegInfo {
            task_id,
            peer,
            kind: *kind,
            phase: *phase,
            mine,
            fraction,
        },
        now,
    )
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
        .is_some_and(|rec| rec.state == TaskState::Terminal)
    {
        return false;
    }

    if matches!(leg.phase, TaskPhase::Offer) {
        // The opening leg mints the record; a duplicate offer just touches it.
        tasks
            .entry(leg.task_id.clone())
            .or_insert_with(|| TaskRecord {
                peer: leg.peer.clone(),
                kind: leg.kind,
                role: if leg.mine {
                    TaskRole::Initiator
                } else {
                    TaskRole::Receiver
                },
                state: TaskState::Proposed,
                last_activity: now,
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

    advance(rec, leg.phase, leg.mine);
    if let Some(fraction) = leg.fraction {
        rec.last_fraction = Some(fraction);
    }
    rec.last_activity = now;

    // Content legs (everything but the plumbing `Progress`) burn the budget.
    let mut over_cap = false;
    if crate::protocol::message::is_content_phase(leg.phase) {
        rec.content_count = rec.content_count.saturating_add(1);
        over_cap = rec.content_count == TASK_CONTENT_CAP + 1;
    }
    over_cap
}

/// The per-phase coarse transition (state + ball). Illegal/out-of-order
/// phases for the current state are silently ignored (the conservative,
/// no-consensus rule).
fn advance(rec: &mut TaskRecord, phase: TaskPhase, mine: bool) {
    let sender = if mine { rec.role } else { rec.role.opposite() };
    match phase {
        TaskPhase::Accept if rec.state == TaskState::Proposed => {
            // The receiver commits and keeps the ball (it now does the work).
            rec.state = TaskState::Active;
            rec.ball = TaskRole::Receiver;
        }
        TaskPhase::Context if matches!(rec.state, TaskState::Proposed | TaskState::Active) => {
            // A question/answer: advance into the exchange, flip the ball to
            // whoever now owes the reply.
            rec.state = TaskState::Active;
            rec.ball = sender.opposite();
        }
        TaskPhase::Done if rec.state == TaskState::Active => {
            // Request to close → the initiator reviews.
            rec.state = TaskState::Review;
            rec.ball = TaskRole::Initiator;
        }
        TaskPhase::Change if rec.state == TaskState::Review => {
            // Reviewer wants revisions → back to the receiver.
            rec.state = TaskState::Active;
            rec.ball = TaskRole::Receiver;
        }
        TaskPhase::Confirm if rec.state == TaskState::Review => rec.state = TaskState::Terminal,
        TaskPhase::Decline if matches!(rec.state, TaskState::Proposed | TaskState::Active) => {
            rec.state = TaskState::Terminal;
        }
        TaskPhase::Cancel => rec.state = TaskState::Terminal,
        // No-op here: `Offer` is handled at record creation, `Progress` is
        // liveness-only, and every other variant lands here only when its
        // guard above failed (an out-of-order/backward leg, conservatively
        // ignored). Listed explicitly so a new phase forces a decision.
        TaskPhase::Offer
        | TaskPhase::Progress
        | TaskPhase::Accept
        | TaskPhase::Context
        | TaskPhase::Done
        | TaskPhase::Change
        | TaskPhase::Confirm
        | TaskPhase::Decline => {}
    }
}

/// Sweep the registry for tasks idle past the debounce timeout, then
/// **garbage-collect** records that have been terminal longer than the
/// timeout. Each eviction freezes the record, emits a `task_timeout` event,
/// and broadcasts a terminal `Cancel` so the peer converges; the GC pass
/// keeps `state.tasks` bounded (a terminal record older than the timeout is
/// past the dedup window — no further leg for that `task_id` will arrive, so
/// it is safe to drop). The task analogue of
/// [`crate::lifecycle::heartbeat::tick_sweep`].
pub(crate) async fn tick_task_sweep(
    state: &mut EventLoopState,
    sender: &GossipSender,
    swarm: &SwarmId,
    author: &Nickname,
    out: &output::Output,
) {
    let now = Instant::now();
    let timeout = Duration::from_secs(task_timeout_secs());
    let expired: Vec<(TaskId, Nickname, TaskKind)> = state
        .tasks
        .iter()
        .filter(|(_, rec)| {
            rec.state != TaskState::Terminal && now.duration_since(rec.last_activity) > timeout
        })
        .map(|(task_id, rec)| (task_id.clone(), rec.peer.clone(), rec.kind))
        .collect();

    for (task_id, peer, kind) in expired {
        if let Some(rec) = state.tasks.get_mut(&task_id) {
            rec.state = TaskState::Terminal;
        }
        out.task_timeout(&task_id);
        tracing::debug!(%task_id, %peer, "task evicted (idle-debounce timeout)");
        broadcast_leg(
            state,
            sender,
            swarm,
            author,
            &peer,
            &task_id,
            kind,
            TaskPhase::Cancel,
            "timeout",
        )
        .await;
    }

    // GC: drop terminal records past the dedup window so the registry stays
    // bounded over a long-lived daemon's task churn (the analogue of the
    // heartbeat sweep pruning `quiet_since`).
    state.tasks.retain(|_, rec| {
        rec.state != TaskState::Terminal || now.duration_since(rec.last_activity) <= timeout
    });
}

/// Emit a `Progress` keepalive for every live task whose ball we hold and
/// that we've gone quiet on past the keepalive cadence — so a silent owner
/// (deciding, executing, or reviewing) does not wrongly time out. The task
/// analogue of [`crate::lifecycle::heartbeat::tick_alive`].
pub(crate) async fn tick_task_keepalive(
    state: &mut EventLoopState,
    sender: &GossipSender,
    swarm: &SwarmId,
    author: &Nickname,
) {
    /// `(task_id, peer, kind, last_fraction)` for one keepalive-due task.
    type KeepaliveDue = (TaskId, Nickname, TaskKind, Option<(u64, u64)>);

    let now = Instant::now();
    let interval = Duration::from_secs(task_keepalive_secs());
    let due: Vec<KeepaliveDue> = state
        .tasks
        .iter()
        .filter(|(_, rec)| {
            rec.state != TaskState::Terminal
                && rec.i_own_ball()
                && now.duration_since(rec.last_activity) >= interval
        })
        .map(|(task_id, rec)| {
            (
                task_id.clone(),
                rec.peer.clone(),
                rec.kind,
                rec.last_fraction,
            )
        })
        .collect();

    for (task_id, peer, kind, fraction) in due {
        let body = match fraction {
            Some((done, total)) => format!("{done}/{total}"),
            None => String::new(),
        };
        broadcast_leg(
            state,
            sender,
            swarm,
            author,
            &peer,
            &task_id,
            kind,
            TaskPhase::Progress,
            &body,
        )
        .await;
        if let Some(rec) = state.tasks.get_mut(&task_id) {
            rec.last_activity = Instant::now();
        }
    }
}

/// Build, sign, and fire-and-forget a daemon-originated task leg (the
/// keepalive `Progress` and the timeout `Cancel`). Unsigned-body validation
/// can't fail for our short literals, so a serialize error is swallowed like
/// any other plumbing broadcast.
#[expect(clippy::too_many_arguments, reason = "a signed task leg's fields")]
async fn broadcast_leg(
    state: &EventLoopState,
    sender: &GossipSender,
    swarm: &SwarmId,
    author: &Nickname,
    peer: &Nickname,
    task_id: &TaskId,
    kind: TaskKind,
    phase: TaskPhase,
    body: &str,
) {
    let Ok(body) = MessageBody::new(body) else {
        return;
    };
    let msg = Message::new_task(
        swarm,
        author,
        peer.clone(),
        task_id.clone(),
        kind,
        phase,
        body,
    )
    .signed(&state.identity);
    if let Ok(bytes) = msg.serialize() {
        let _ = sender.broadcast(Bytes::from(bytes)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{LegInfo, TaskRole, TaskState, apply};
    use crate::protocol::{Nickname, TaskId, TaskKind, TaskPhase};
    use std::collections::HashMap;
    use std::time::Instant;

    fn tid() -> TaskId {
        TaskId::from("550e8400-e29b-41d4-a716-446655440000")
    }

    fn leg(phase: TaskPhase, mine: bool) -> LegInfo<'static> {
        // Leak a fixed peer/task_id for 'static convenience in tests.
        LegInfo {
            task_id: Box::leak(Box::new(tid())),
            peer: Box::leak(Box::new(Nickname::from("calm-otter"))),
            kind: TaskKind::Handover,
            phase,
            mine,
            fraction: None,
        }
    }

    /// Drive the happy-path lifecycle from the initiator's view and assert
    /// the coarse state + ball-owner at each step.
    #[test]
    fn initiator_happy_path_states_and_ball() {
        let mut tasks = HashMap::new();
        let now = Instant::now();

        apply(&mut tasks, &leg(TaskPhase::Offer, true), now);
        let rec = &tasks[tid().as_str()];
        assert_eq!(rec.role, TaskRole::Initiator);
        assert_eq!(rec.state, TaskState::Proposed);
        assert_eq!(rec.ball, TaskRole::Receiver);

        apply(&mut tasks, &leg(TaskPhase::Accept, false), now);
        assert_eq!(tasks[tid().as_str()].state, TaskState::Active);
        assert_eq!(tasks[tid().as_str()].ball, TaskRole::Receiver);

        // Receiver asks a question → ball flips to me (the initiator).
        apply(&mut tasks, &leg(TaskPhase::Context, false), now);
        assert_eq!(tasks[tid().as_str()].ball, TaskRole::Initiator);

        apply(&mut tasks, &leg(TaskPhase::Done, false), now);
        assert_eq!(tasks[tid().as_str()].state, TaskState::Review);
        assert_eq!(tasks[tid().as_str()].ball, TaskRole::Initiator);

        apply(&mut tasks, &leg(TaskPhase::Confirm, true), now);
        assert_eq!(tasks[tid().as_str()].state, TaskState::Terminal);
    }

    /// A terminal task is frozen: a later leg never mutates it.
    #[test]
    fn terminal_is_frozen() {
        let mut tasks = HashMap::new();
        let now = Instant::now();
        apply(&mut tasks, &leg(TaskPhase::Offer, true), now);
        apply(&mut tasks, &leg(TaskPhase::Decline, false), now);
        assert_eq!(tasks[tid().as_str()].state, TaskState::Terminal);
        // A stray confirm after decline must not resurrect it.
        apply(&mut tasks, &leg(TaskPhase::Confirm, true), now);
        assert_eq!(tasks[tid().as_str()].state, TaskState::Terminal);
    }

    /// A non-offer leg for an unknown task id is dropped (no record created).
    #[test]
    fn unknown_task_non_offer_is_ignored() {
        let mut tasks = HashMap::new();
        apply(&mut tasks, &leg(TaskPhase::Context, false), Instant::now());
        assert!(tasks.is_empty());
    }

    /// Out-of-order legs (a `Done` before `Accept`) are ignored, not applied.
    #[test]
    fn out_of_order_is_ignored() {
        let mut tasks = HashMap::new();
        let now = Instant::now();
        apply(&mut tasks, &leg(TaskPhase::Offer, true), now);
        apply(&mut tasks, &leg(TaskPhase::Done, false), now);
        // Still Proposed — the premature Done did nothing.
        assert_eq!(tasks[tid().as_str()].state, TaskState::Proposed);
    }

    /// Content legs count toward the cap; `Progress` does not, and the cap
    /// crossing is reported exactly once.
    #[test]
    fn content_cap_excludes_progress() {
        let mut tasks = HashMap::new();
        let now = Instant::now();
        apply(&mut tasks, &leg(TaskPhase::Offer, true), now); // count 1
        apply(&mut tasks, &leg(TaskPhase::Accept, false), now); // count 2
        for _ in 0..50 {
            // Progress beats never increment the content count.
            apply(&mut tasks, &leg(TaskPhase::Progress, false), now);
        }
        assert_eq!(tasks[tid().as_str()].content_count, 2);
        // Drive to exactly the cap, then one past it.
        let mut over = false;
        while tasks[tid().as_str()].content_count < super::TASK_CONTENT_CAP {
            over |= apply(&mut tasks, &leg(TaskPhase::Context, false), now);
        }
        assert!(!over, "no crossing while at/under the cap");
        let crossing = apply(&mut tasks, &leg(TaskPhase::Context, false), now);
        assert!(crossing, "the leg that pushes past the cap reports it");
    }
}
