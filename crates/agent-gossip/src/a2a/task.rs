//! The daemon-side **task** state machine + its two timers, over the A2A
//! lifecycle.
//!
//! A task is a directed, multi-leg conversation whose wire form is pure A2A:
//! a `message/send` (no `taskId`) opens it — the worker mints the id and
//! returns the `Task` — then `TaskStatusUpdate`s and mid-task `message/send`s
//! advance it, a `TaskArtifactUpdate` returns the result, and the **worker's**
//! `completed` status closes it (after the initiator's approval message). The
//! daemon owns only the *coarse* lifecycle — state advance, the per-task idle
//! debounce, and the per-task keepalive — while the skill owns the
//! *content*.
//!
//! The machine is **distributed** with no consensus: each party derives
//! state from the legs it has seen, so the rules are deliberately
//! conservative — **monotonic** advance (a leg that would move backward is
//! ignored), **idempotent** (a duplicate leg is a no-op), and a terminal
//! record is frozen. Local triggers (the timeout) *broadcast* a terminal
//! `canceled` status so the other side converges.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use habilis_network::embed::{EventLoopState, HandlerCtx};
use habilis_network::protocol::{Message, MessageId, MessageKind, Nickname, UnixSecs};
use habilis_network::util::bounded_fifo_set::BoundedFifoSet;

use super::gossip::ReaderBeat;
use super::{META_REASON, TaskId, TaskState, gossip, wire};
use crate::a2a::app::A2aApp;
use crate::a2a::tuning::{
    TASK_SURFACED_LEGS_CAP, task_keepalive_secs, task_skill_silence_max_secs,
    task_stall_check_secs, task_timeout_secs,
};
use crate::output;

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

/// When the status leg that set or cleared `waiting` was sent: its wire second,
/// and its order value when its peer sent one.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WaitingMark {
    at: UnixSecs,
    order: Option<u64>,
}

impl WaitingMark {
    fn new(at: UnixSecs, order: Option<u64>) -> Self {
        Self { at, order }
    }
}

/// One in-flight task this node is a party to.
#[derive(Debug)]
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
    /// the daemon's own keepalive) — the terminal GC reads this, never the wire
    /// `ts` (which can skew).
    pub last_activity: Instant,
    /// Local-clock instant of our own last leg or beat. The keepalive cadence
    /// reads this: on `last_activity` the peer's beats kept ours quiet.
    pub last_sent: Instant,
    /// Local-clock instant of the last leg or beat **from the counterparty**.
    /// The idle sweep reads this, not `last_activity`: both sides beat, so on
    /// the shared clock our own beats would keep a task alive after the peer
    /// daemon died.
    pub last_peer_activity: Instant,
    /// Local-clock instant of the last leg an agent sent, on either side.
    /// Beats never touch it (an RPC snapshot in `adopt_initiator` does), so it
    /// measures how long both agents have left the task alone, and a forgotten
    /// task stops being beaten.
    pub last_skill_activity: Instant,
    /// Whether the counterparty has beaten this task at least once. A 0.8/0.9
    /// peer beats only while it owes the next move, so until it has, its
    /// silence cannot be read as death.
    pub peer_beats: bool,
    /// Ids of the peer's legs already surfaced for this task. The engine
    /// forgets a frame id after a bounded count, and anti-entropy then serves
    /// the same leg again, so the app remembers what it has shown. Beats are
    /// never recorded: at one every 30 s they would evict every real id.
    pub surfaced_legs: BoundedFifoSet<MessageId>,
    /// Last progress fraction reported, replayed on keepalives.
    pub last_fraction: Option<(u64, u64)>,
    /// Which role currently owes the next move.
    pub ball: TaskRole,
    /// Why the worker waits on its own user, from the `mesh:waiting` key of its
    /// last status leg. Any other status leg clears it.
    pub waiting: Option<String>,
    /// The status leg that last set or cleared `waiting`. A leg older than this
    /// is a replay and changes nothing. A worker clock that steps back drops
    /// newer waiting updates until it passes this time again, the same exposure
    /// as `is_stale`, accepted.
    pub waiting_mark: Option<WaitingMark>,
    /// When this node last told its agent that the worker's reader is silent,
    /// so one task raises one event per interval.
    pub last_stall_event: Option<Instant>,
}

impl TaskRecord {
    /// Both parties beat every live task, whoever holds the ball: a human
    /// deciding, a reviewer reading, and a long build all look like silence,
    /// and each lost tasks at ~4 min. A beat proves the daemon is up, and the
    /// engine's orphan watch quits the daemon when its agent session dies. The
    /// silence cap covers the one case that watch cannot see: a live session
    /// whose agent forgot the task.
    fn should_keepalive(&self, now: Instant, cadence: Duration) -> bool {
        !self.state.is_terminal()
            && now.duration_since(self.last_sent) >= cadence
            && !self.skill_silence_exceeded(now)
    }

    fn skill_silence_exceeded(&self, now: Instant) -> bool {
        now.duration_since(self.last_skill_activity)
            > Duration::from_secs(task_skill_silence_max_secs())
    }
}

/// What the initiator's daemon tells its agent when the worker's reader is
/// silent: facts only, nothing is canceled.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct StallNotice {
    pub task_id: TaskId,
    pub peer: Nickname,
    pub last_poll_secs: u64,
    pub check_secs: u64,
}

/// Whether a worker beat shows a silent reader on a task we opened. A worker
/// that waits on its own user (`waiting`) or holds a bell is reading, so it
/// never matches. One notice per task per interval.
fn stalled(reader: ReaderBeat, rec: &TaskRecord, interval: Duration, now: Instant) -> bool {
    rec.role == TaskRole::Initiator
        && rec.state == TaskState::Working
        && rec.waiting.is_none()
        && !reader.bell
        && reader.last_poll_secs >= interval.as_secs()
        && rec
            .last_stall_event
            .is_none_or(|at| now.duration_since(at) >= interval)
}

/// Check the reader keys of a fresh inbound beat (the value [`ingest`] returns)
/// for a silent worker, and mark the task so the next notice waits one
/// interval. A beat from an old peer carries no keys, so it never gets here.
pub(crate) fn stall_notice(
    tasks: &mut HashMap<TaskId, TaskRecord>,
    task_id: &TaskId,
    reader: ReaderBeat,
    now: Instant,
) -> Option<StallNotice> {
    let rec = tasks.get_mut(task_id)?;
    let check_secs = task_stall_check_secs();
    if !stalled(reader, rec, Duration::from_secs(check_secs), now) {
        return None;
    }
    rec.last_stall_event = Some(now);
    Some(StallNotice {
        task_id: task_id.clone(),
        peer: rec.peer.clone(),
        last_poll_secs: reader.last_poll_secs,
        check_secs,
    })
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
///
/// Returns the worker's [`ReaderBeat`] when the frame is a fresh inbound beat
/// that carries one, so the caller can check it with [`stall_notice`] without
/// parsing the frame again.
pub(crate) fn ingest(
    tasks: &mut HashMap<TaskId, TaskRecord>,
    params: IngestLegParams<'_>,
) -> Option<ReaderBeat> {
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
        return None;
    };
    let mut waiting = None;
    let mut order = None;
    let mut reader = None;
    let (kind, fraction) = match tag.as_str() {
        wire::STATUS => {
            let Ok(payload) = gossip::status_payload(frame) else {
                return None;
            };
            if gossip::is_beat(&payload) {
                reader = gossip::beat_reader(&payload);
                (LegKind::Beat, gossip::beat_fraction(&payload))
            } else {
                if payload.status.state == TaskState::Working {
                    waiting = gossip::status_waiting(&payload);
                }
                order = gossip::status_order(&payload);
                (LegKind::Status(payload.status.state), None)
            }
        }
        wire::ARTIFACT => (LegKind::Artifact, None),
        _ => return None,
    };
    if kind == LegKind::Beat && !mine && is_stale(frame) {
        return None;
    }
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
    // Every status leg restates whether the worker waits, so a plain one clears
    // it. Anti-entropy replays old legs, so two checks gate the waiting update
    // only: `is_newer_than_waiting` drops a leg older than the last answer, and
    // `surfaced_legs` drops a leg already shown, which covers a replay in the
    // same second as the answer. `apply` above still runs for every leg and
    // `advance` is idempotent. A finished task waits for nothing.
    if matches!(kind, LegKind::Status(_))
        && let Some(rec) = tasks.get_mut(task_id)
    {
        if rec.state.is_terminal() {
            rec.waiting = None;
        } else if is_newer_than_waiting(rec, frame.timestamp, order)
            && !rec.surfaced_legs.contains(&frame.id)
        {
            rec.waiting = waiting;
            rec.waiting_mark = Some(WaitingMark::new(frame.timestamp, order));
            // Remembered here, not only by `note_surfaced`: a replay in the
            // same second as this leg must not pass the clock check again.
            rec.surfaced_legs.insert(frame.id.clone());
        }
    }
    // Only a beat from the other party says anything about its reader.
    reader.filter(|_| !mine)
}

/// Whether a status leg is not older than the one that last set `waiting`. When
/// both legs carry an order value it decides, so two legs of one second stay in
/// the order they were sent. Otherwise the wire second decides, as before.
fn is_newer_than_waiting(rec: &TaskRecord, at: UnixSecs, order: Option<u64>) -> bool {
    rec.waiting_mark
        .is_none_or(|mark| match (order, mark.order) {
            (Some(order), Some(last)) => order >= last,
            _ => at >= mark.at,
        })
}

/// A beat older than one idle timeout proves nothing about the peer now. The
/// engine dedups by a bounded count, not by age, so a relay could re-inject an
/// old beat after the peer died. The bound is coarse on purpose, to tolerate
/// clock skew between hosts.
fn is_stale(frame: &Message) -> bool {
    UnixSecs::now().since(frame.timestamp).as_secs() > task_timeout_secs()
}

/// What a reaped task keeps: enough to answer its own peer's `GetTask`, not
/// the full record.
pub(crate) struct ClosedTask {
    pub peer: Nickname,
    pub state: TaskState,
}

/// The reaped tasks, oldest dropped first once `cap` is reached. A map, because
/// `BoundedFifoSet` keeps only ids and does not say what it evicted.
pub(crate) struct ClosedTasks {
    cap: usize,
    order: VecDeque<TaskId>,
    by_id: HashMap<TaskId, ClosedTask>,
}

impl ClosedTasks {
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            cap,
            order: VecDeque::new(),
            by_id: HashMap::new(),
        }
    }

    pub(crate) fn insert(&mut self, task_id: TaskId, closed: ClosedTask) {
        if self.by_id.insert(task_id.clone(), closed).is_none() {
            self.order.push_back(task_id);
        }
        while self.order.len() > self.cap {
            if let Some(oldest) = self.order.pop_front() {
                self.by_id.remove(&oldest);
            }
        }
    }

    pub(crate) fn get(&self, task_id: &TaskId) -> Option<&ClosedTask> {
        self.by_id.get(task_id)
    }
}

/// Whether an inbound leg was already shown, and must stay off the surface: its
/// task closed (live record terminal, or reaped and remembered in `closed`), or
/// this leg id was surfaced before. An unknown task still surfaces: the
/// initiator's record comes from the RPC response, and the worker's first leg
/// can arrive ahead of it.
pub(crate) fn is_replay(
    tasks: &HashMap<TaskId, TaskRecord>,
    closed: &ClosedTasks,
    task_id: &TaskId,
    leg_id: &MessageId,
) -> bool {
    if closed.get(task_id).is_some() {
        return true;
    }
    tasks
        .get(task_id)
        .is_some_and(|rec| rec.state.is_terminal() || rec.surfaced_legs.contains(leg_id))
}

/// Remember a leg that was surfaced, so a later copy of it is a replay. A beat
/// is skipped: at one every 30 s, beats would evict every real leg id.
pub(crate) fn note_surfaced(
    tasks: &mut HashMap<TaskId, TaskRecord>,
    task_id: &TaskId,
    frame: &Message,
) {
    let is_beat = gossip::status_payload(frame).is_ok_and(|payload| gossip::is_beat(&payload));
    if is_beat {
        return;
    }
    if let Some(rec) = tasks.get_mut(task_id) {
        rec.surfaced_legs.insert(frame.id.clone());
    }
}

/// Whether the registry has room for one more task offered by a peer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    Ok,
    /// That peer already holds its allowance of live tasks with us.
    PeerAtCap,
    /// The registry is full across every peer.
    RegistryFull,
}

/// May `peer` open one more task with us?
///
/// A task offer is the only registry entry a remote peer mints directly, so
/// this is where `tasks` is kept bounded — the discipline every other
/// registry already follows (`register_a2a_waiter`, `register_poll_waiter`).
/// The per-peer quota is the load-bearing one: a global cap alone would let a
/// single flooder fill the registry and lock every honest peer out, which
/// turns a leak into a denial of service. The global cap is the memory
/// backstop behind it, since nothing stops an attacker minting fresh
/// identities.
pub(crate) fn admit_new_task(tasks: &HashMap<TaskId, TaskRecord>, peer: &Nickname) -> Admission {
    if tasks.len() >= crate::a2a::tuning::TASKS_CAP {
        return Admission::RegistryFull;
    }
    let live_with_peer = tasks
        .values()
        .filter(|rec| rec.peer == *peer && !rec.state.is_terminal())
        .count();
    if live_with_peer >= crate::a2a::tuning::TASKS_PER_PEER_CAP {
        return Admission::PeerAtCap;
    }
    Admission::Ok
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
        Entry::Vacant(slot) => {
            // First adoption (task creation): open the initiator-side record at
            // the server-minted state.
            slot.insert(TaskRecord {
                peer: peer.clone(),
                role: TaskRole::Initiator,
                state: task_state,
                review: false,
                last_activity: now,
                last_sent: now,
                last_peer_activity: now,
                last_skill_activity: now,
                peer_beats: false,
                surfaced_legs: BoundedFifoSet::new(TASK_SURFACED_LEGS_CAP),
                last_fraction: None,
                waiting: None,
                waiting_mark: None,
                last_stall_event: None,
                // Whoever owes the next move: the worker on a live task, us if
                // it parked for our input.
                ball: if task_state == TaskState::InputRequired {
                    TaskRole::Initiator
                } else {
                    TaskRole::Receiver
                },
            });
        }
        Entry::Occupied(mut slot) => {
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
            rec.last_peer_activity = now;
            rec.last_skill_activity = now;
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
                last_sent: now,
                last_peer_activity: now,
                last_skill_activity: now,
                peer_beats: false,
                surfaced_legs: BoundedFifoSet::new(TASK_SURFACED_LEGS_CAP),
                last_fraction: None,
                waiting: None,
                waiting_mark: None,
                last_stall_event: None,
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
    if leg.mine {
        rec.last_sent = now;
    } else {
        rec.last_peer_activity = now;
    }
    if leg.kind == LegKind::Beat {
        rec.peer_beats |= !leg.mine;
    } else {
        rec.last_skill_activity = now;
    }
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
        LegKind::Status(TaskState::Completed)
            if sender == TaskRole::Receiver
                && matches!(rec.state, TaskState::Working | TaskState::InputRequired) =>
        {
            // Native A2A: the worker (server) drives its own task to
            // `completed`, after the initiator's approval message.
            //
            // Not from `Submitted`: a task closed before its worker ever
            // accepted delivered nothing, and terminal records are frozen, so
            // the initiator is left with a record that reads exactly like a
            // real completion and can never be reopened. `Failed` deliberately
            // has no such guard — declining an offer outright is the skills'
            // decline path.
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
    let (expired, reaped) = sweep_registry(&mut app.tasks, &mut app.closed_tasks, now, timeout);

    for (task_id, peer) in &expired {
        out.task_timeout(task_id, output::TaskGoneReason::Timeout);
        tracing::debug!(%task_id, %peer, "task evicted (idle-debounce timeout)");
    }
    // A task a late reply opened is unknown to the caller's agent, so it gets
    // no `task_timeout` line — only the worker is told.
    let late_cancels = app.take_late_cancels();
    let cancels = expired
        .iter()
        .map(|(task_id, peer)| (task_id, peer, "timeout"))
        .chain(
            late_cancels
                .iter()
                .map(|(task_id, peer)| (task_id, peer, "call-timed-out")),
        );
    for (task_id, peer, reason) in cancels {
        let update = gossip::status_update(
            ctx.mesh,
            gossip::StatusUpdateParams {
                task_id,
                state: TaskState::Canceled,
                note: None,
                metadata: Some(serde_json::json!({ META_REASON: reason })),
            },
        );
        broadcast_status(
            state,
            ctx,
            BroadcastStatusParams {
                peer,
                task_id,
                update: &update,
            },
        )
        .await;
    }

    // A reaped task's offloaded blobs go with it — unlink their spool files
    // (the review window has long closed).
    let call_blobs = app.take_call_blobs(&reaped, now);
    if let Some(server) = app.blob_server.as_ref() {
        for task_id in &reaped {
            server
                .evict_content(&habilis_network::ops::blob::ContentId::new(
                    task_id.as_str(),
                ))
                .await;
        }
        for group in &call_blobs {
            server.evict_content(group).await;
        }
    }
    // Logged after the eviction, so a reader of this line knows the blobs are gone.
    for task_id in &reaped {
        tracing::info!(target: "agent_gossip::a2a", %task_id, "task reaped");
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
    closed: &mut ClosedTasks,
    now: Instant,
    timeout: Duration,
) -> (Vec<(TaskId, Nickname)>, Vec<TaskId>) {
    let mut expired = Vec::new();
    for (task_id, rec) in tasks.iter_mut() {
        let peer_silent = now.duration_since(rec.last_peer_activity) > timeout;
        let peer_gone = rec.peer_beats || rec.skill_silence_exceeded(now);
        if !rec.state.is_terminal() && peer_silent && peer_gone {
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
    for task_id in &reaped {
        if let Some(rec) = tasks.remove(task_id) {
            closed.insert(
                task_id.clone(),
                ClosedTask {
                    peer: rec.peer,
                    state: rec.state,
                },
            );
        }
    }
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

/// Emit a beat (keepalive) status for every live task we have gone quiet on
/// past the keepalive cadence. The task analogue of the engine's own lifecycle
/// keepalive tick. See [`TaskRecord::should_keepalive`].
pub(crate) async fn tick_task_keepalive(
    state: &mut EventLoopState,
    app: &mut A2aApp,
    ctx: &HandlerCtx<'_>,
) {
    /// `(task_id, peer, state, last_fraction)` for one keepalive-due task.
    type KeepaliveDue = (TaskId, Nickname, TaskState, Option<(u64, u64)>);

    let now = Instant::now();
    let cadence = Duration::from_secs(task_keepalive_secs());
    let (bell, last_poll) = super::node::reader_liveness(app, ctx);
    let reader = last_poll.map(|last_poll_secs| ReaderBeat {
        bell,
        last_poll_secs,
    });
    let due: Vec<KeepaliveDue> = app
        .tasks
        .iter()
        .filter(|(_, rec)| rec.should_keepalive(now, cadence))
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
                metadata: Some(gossip::beat_metadata_with_reader(fraction, reader)),
            },
        );
        let sent = broadcast_status(
            state,
            ctx,
            BroadcastStatusParams {
                peer: &peer,
                task_id: &task_id,
                update: &update,
            },
        )
        .await;
        if sent && let Some(rec) = app.tasks.get_mut(&task_id) {
            let sent_at = Instant::now();
            rec.last_activity = sent_at;
            rec.last_sent = sent_at;
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
/// Returns whether the send started, so a beat that could not start is not
/// counted as sent and retries on the next tick instead of a full cadence
/// later. A send that starts and then fails costs one cadence.
async fn broadcast_status(
    state: &EventLoopState,
    ctx: &HandlerCtx<'_>,
    params: BroadcastStatusParams<'_>,
) -> bool {
    let BroadcastStatusParams {
        peer,
        task_id: _task_id,
        update,
    } = params;
    let Ok(body) = gossip::payload_body(update) else {
        return false;
    };
    // Directed status frames are sealed to the peer like every other directed
    // frame (the receive path always unseals a directed body). If the peer's key
    // isn't known yet, skip this beat/cancel — it is fire-and-forget plumbing and
    // a later one retries.
    let Ok(body) = crate::a2a::send::seal_directed(state, peer, &body) else {
        return false;
    };
    let kind = MessageKind::App {
        tag: habilis_network::protocol::AppTag::from(wire::STATUS),
        to: Some(peer.clone()),
        corr: None,
    };
    let msg = Message::new_frame(ctx.mesh, ctx.author, kind, body).sign(state.identity());
    let Ok(bytes) = msg.serialize() else {
        return false;
    };
    // Through `deliver`, not `sender.broadcast`: the frame is directed, so
    // it takes unicast to `peer` like every other directed frame. Straight
    // onto gossip it would flood `author`/`to` in the clear on a timer, and
    // every bystander would run `lifecycle::observe` on a beat meant for
    // one peer — waking a parked bell off someone else's task. In the
    // background because this runs from `on_tick`, inline on the event loop:
    // a cold `deliver` dials there and stops the whole node for the dial and
    // path-select budgets, once per due task.
    habilis_network::ops::deliver_in_background(&msg, bytes, state, ctx.sender).await
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use habilis_network::protocol::{Nickname, UnixSecs};

    use super::{IngestLegParams, LegInfo, LegKind, TaskRecord, TaskRole, TaskState, apply};
    use crate::a2a::TaskId;
    use crate::a2a::gossip::ReaderBeat;

    fn tid() -> TaskId {
        TaskId::from("550e8400-e29b-41d4-a716-446655440000")
    }

    /// Every live task is beaten once quiet past the cadence — on either side,
    /// whoever holds the ball, however long the skill has been silent. Pre-fix
    /// only the ball-owner beat, and only within 120 s of its skill's last leg,
    /// so an accept question, a review, or a long build lost the task.
    #[test]
    fn keepalive_covers_every_live_task_past_the_cadence() {
        let cadence = Duration::from_secs(30);
        let now = Instant::now();
        let quiet = now.checked_sub(2 * cadence).unwrap();

        // I initiated it and the worker holds the ball.
        let mut tasks = HashMap::new();
        apply(&mut tasks, &leg(LegKind::Offer, true), quiet);
        let rec = tasks.get_mut(&tid()).unwrap();
        assert_eq!(rec.ball, TaskRole::Receiver);
        assert!(
            rec.should_keepalive(now, cadence),
            "the side without the ball beats too"
        );

        // Silent for an hour: still beaten.
        let hour_ago = now.checked_sub(Duration::from_hours(1)).unwrap();
        rec.last_sent = hour_ago;
        assert!(
            rec.should_keepalive(now, cadence),
            "skill silence does not stop the beat"
        );

        // Within the cadence ⇒ not due yet.
        rec.last_sent = now;
        assert!(!rec.should_keepalive(now, cadence));

        // A terminal task is never beaten.
        rec.last_sent = quiet;
        rec.state = TaskState::Canceled;
        assert!(!rec.should_keepalive(now, cadence));
    }

    /// The peer's beat must not stand in for ours. The cadence once read the
    /// clock that inbound legs refresh too, so whichever side beat first kept
    /// the other side quiet, and that side's peer then reaped the task.
    #[test]
    fn a_peer_beat_does_not_suppress_our_beat() {
        let cadence = Duration::from_secs(30);
        let now = Instant::now();
        let mut tasks = HashMap::new();
        apply(
            &mut tasks,
            &leg(LegKind::Offer, true),
            now.checked_sub(2 * cadence).unwrap(),
        );
        apply(&mut tasks, &leg(LegKind::Beat, false), now);
        assert!(tasks[&tid()].should_keepalive(now, cadence));
    }

    /// A task no agent has touched for the silence cap stops being beaten, so
    /// the peer reaps it. Without the cap, a task forgotten after a `/clear`
    /// was beaten for as long as both daemons ran, and 64 of them filled the
    /// per-peer quota.
    #[test]
    fn a_forgotten_task_stops_beating_after_the_silence_cap() {
        let cadence = Duration::from_secs(30);
        let now = Instant::now();
        let day_ago = now.checked_sub(Duration::from_hours(25)).unwrap();
        let mut tasks = HashMap::new();
        apply(&mut tasks, &leg(LegKind::Offer, true), day_ago);
        // Only the daemons have spoken since: the peer's beat, and ours.
        apply(&mut tasks, &leg(LegKind::Beat, false), now);
        tasks.get_mut(&tid()).unwrap().last_sent = day_ago;
        assert!(!tasks[&tid()].should_keepalive(now, cadence));
    }

    /// A 0.8/0.9 peer beats only while it owes the next move, so its silence
    /// says nothing about whether it lives. Reaping it after one idle timeout
    /// cancelled a working task 2 min after accept, sooner than before the
    /// heartbeat.
    #[test]
    fn a_peer_that_never_beat_is_not_reaped_for_silence() {
        let timeout = Duration::from_mins(2);
        let now = Instant::now();
        let accepted = now.checked_sub(timeout * 2).unwrap();
        let mut tasks = HashMap::new();
        apply(&mut tasks, &leg(LegKind::Offer, false), accepted);
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Working), true),
            accepted,
        );

        let (expired, _) = super::sweep_registry(&mut tasks, &mut closed(), now, timeout);
        assert!(expired.is_empty(), "an old peer's silence is not death");
    }

    /// The silence cap still ends a task with a peer that never beat.
    #[test]
    fn a_peer_that_never_beat_is_reaped_after_the_silence_cap() {
        let timeout = Duration::from_mins(2);
        let now = Instant::now();
        let day_ago = now.checked_sub(Duration::from_hours(25)).unwrap();
        let mut tasks = HashMap::new();
        apply(&mut tasks, &leg(LegKind::Offer, false), day_ago);

        let (expired, _) = super::sweep_registry(&mut tasks, &mut closed(), now, timeout);
        assert_eq!(expired.len(), 1);
    }

    /// A beat is only evidence while it is fresh. The engine dedups by a
    /// bounded count, not by age, so a relay that re-injects an old beat after
    /// the peer died must not keep the task alive.
    #[test]
    fn a_stale_beat_does_not_refresh_the_peer_clock() {
        use habilis_network::protocol::{AppFrameParams, AppTag, MeshId, Message, MessageBody};

        let mesh = MeshId::from("test");
        let now = Instant::now();
        let mut tasks = HashMap::new();
        apply(&mut tasks, &leg(LegKind::Offer, true), now);
        let peer_clock = tasks[&tid()].last_peer_activity;

        let payload = crate::a2a::gossip::status_update(
            &mesh,
            crate::a2a::gossip::StatusUpdateParams {
                task_id: &tid(),
                state: TaskState::Working,
                note: None,
                metadata: Some(crate::a2a::gossip::beat_metadata(None)),
            },
        );
        let mut beat = Message::new_app(
            &mesh,
            &Nickname::from("calm-otter"),
            AppFrameParams {
                tag: AppTag::from(crate::a2a::wire::STATUS),
                to: Some(Nickname::from("worker-bot")),
                corr: None,
                body: MessageBody::new(serde_json::to_string(&payload).unwrap()).unwrap(),
            },
        );
        beat.timestamp = beat.timestamp - Duration::from_hours(1);

        let later = now.checked_add(Duration::from_mins(1)).unwrap();
        super::ingest(
            &mut tasks,
            IngestLegParams {
                frame: &beat,
                task_id: &tid(),
                mine: false,
                now: later,
            },
        );
        assert_eq!(tasks[&tid()].last_peer_activity, peer_clock);
    }

    fn closed() -> super::ClosedTasks {
        super::ClosedTasks::new(8)
    }

    /// An inbound status frame for `tid()`; a beat when `beat` is true.
    fn status_leg(beat: bool) -> habilis_network::protocol::Message {
        status_frame(
            TaskState::Working,
            beat.then(|| crate::a2a::gossip::beat_metadata(None)),
        )
    }

    /// An inbound status frame for `tid()` in `state`, carrying `metadata`.
    fn status_frame(
        state: TaskState,
        metadata: Option<serde_json::Value>,
    ) -> habilis_network::protocol::Message {
        use habilis_network::protocol::{AppFrameParams, AppTag, MeshId, Message, MessageBody};

        let mesh = MeshId::from("test");
        let payload = crate::a2a::gossip::status_update(
            &mesh,
            crate::a2a::gossip::StatusUpdateParams {
                task_id: &tid(),
                state,
                note: None,
                metadata,
            },
        );
        Message::new_app(
            &mesh,
            &Nickname::from("calm-otter"),
            AppFrameParams {
                tag: AppTag::from(crate::a2a::wire::STATUS),
                to: Some(Nickname::from("worker-bot")),
                corr: None,
                body: MessageBody::new(serde_json::to_string(&payload).unwrap()).unwrap(),
            },
        )
    }

    /// A task we opened (initiator) whose worker reported `working`.
    fn working_task_we_opened(now: Instant) -> HashMap<TaskId, TaskRecord> {
        let mut tasks = HashMap::new();
        apply(&mut tasks, &leg(LegKind::Offer, true), now);
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Working), false),
            now,
        );
        assert_eq!(tasks[&tid()].state, TaskState::Working);
        tasks
    }

    const STALL_CHECK: Duration = Duration::from_mins(30);

    fn silent_reader() -> ReaderBeat {
        ReaderBeat {
            bell: false,
            last_poll_secs: STALL_CHECK.as_secs(),
        }
    }

    fn beat_from_worker(reader: Option<ReaderBeat>) -> habilis_network::protocol::Message {
        status_frame(
            TaskState::Working,
            Some(crate::a2a::gossip::beat_metadata_with_reader(None, reader)),
        )
    }

    /// The node's path for an inbound beat: `ingest` hands back the reader keys,
    /// and `stall_notice` checks them.
    fn notice_for(
        tasks: &mut HashMap<TaskId, TaskRecord>,
        beat: &habilis_network::protocol::Message,
        now: Instant,
    ) -> Option<super::StallNotice> {
        let reader = super::ingest(
            tasks,
            IngestLegParams {
                frame: beat,
                task_id: &tid(),
                mine: false,
                now,
            },
        )?;
        super::stall_notice(tasks, &tid(), reader, now)
    }

    /// The initiator flags a worker whose agent has not polled for a check
    /// interval with no bell armed. Pre-fix nothing read the reader keys, so a
    /// wedged agent behind a live daemon looked alive for up to 24 h.
    #[test]
    fn a_silent_reader_on_a_working_task_we_opened_is_stalled() {
        let now = Instant::now();
        let tasks = working_task_we_opened(now);
        assert!(super::stalled(
            silent_reader(),
            &tasks[&tid()],
            STALL_CHECK,
            now
        ));
    }

    /// Anything that shows the worker agent reading, or that the beat cannot
    /// speak for, is never flagged.
    #[test]
    fn a_reading_or_waiting_worker_is_not_stalled() {
        let now = Instant::now();
        let mut tasks = working_task_we_opened(now);
        let rec = tasks.get_mut(&tid()).unwrap();

        let bell = ReaderBeat {
            bell: true,
            ..silent_reader()
        };
        assert!(!super::stalled(bell, rec, STALL_CHECK, now), "bell armed");

        let recent = ReaderBeat {
            last_poll_secs: STALL_CHECK.as_secs() - 1,
            ..silent_reader()
        };
        assert!(
            !super::stalled(recent, rec, STALL_CHECK, now),
            "polled lately"
        );

        rec.waiting = Some("asking its user".to_owned());
        assert!(
            !super::stalled(silent_reader(), rec, STALL_CHECK, now),
            "waiting on its own user"
        );
        rec.waiting = None;

        rec.role = TaskRole::Receiver;
        assert!(
            !super::stalled(silent_reader(), rec, STALL_CHECK, now),
            "only the initiator raises the event"
        );
        rec.role = TaskRole::Initiator;

        rec.state = TaskState::InputRequired;
        assert!(
            !super::stalled(silent_reader(), rec, STALL_CHECK, now),
            "the ball is ours, not the worker's"
        );
    }

    /// One notice per task per interval, then again once the interval passes.
    #[test]
    fn a_stalled_task_raises_one_notice_per_interval() {
        let now = Instant::now();
        let mut tasks = working_task_we_opened(now);
        let beat = beat_from_worker(Some(silent_reader()));

        let first = notice_for(&mut tasks, &beat, now);
        assert_eq!(
            first,
            Some(super::StallNotice {
                task_id: tid(),
                peer: Nickname::from("calm-otter"),
                last_poll_secs: STALL_CHECK.as_secs(),
                check_secs: STALL_CHECK.as_secs(),
            })
        );
        let soon = now + Duration::from_secs(30);
        assert_eq!(notice_for(&mut tasks, &beat, soon), None);
        let later = now + STALL_CHECK;
        assert!(notice_for(&mut tasks, &beat, later).is_some());
    }

    /// A beat from an old peer carries no reader keys, so it is never flagged.
    #[test]
    fn a_beat_without_reader_keys_raises_no_notice() {
        let now = Instant::now();
        let mut tasks = working_task_we_opened(now);
        let beat = beat_from_worker(None);
        assert_eq!(notice_for(&mut tasks, &beat, now), None);
    }

    /// A working status leg for `tid()` at wire time `at`, with a waiting reason
    /// when one is given.
    fn waiting_leg(reason: Option<&str>, at: UnixSecs) -> habilis_network::protocol::Message {
        let mut frame = status_frame(
            TaskState::Working,
            crate::a2a::gossip::waiting_metadata(reason),
        );
        frame.timestamp = at;
        frame
    }

    fn ingest_inbound(
        registry: &mut HashMap<TaskId, TaskRecord>,
        frame: &habilis_network::protocol::Message,
    ) {
        super::ingest(
            registry,
            IngestLegParams {
                frame,
                task_id: &tid(),
                mine: false,
                now: Instant::now(),
            },
        );
    }

    /// Anti-entropy re-serves old status legs hours late. Both the `--waiting`
    /// leg and the plain leg that clears it surface once, so the node records
    /// their ids; their wire time covers the rest. A replay must not set or clear
    /// the reason again, also when the leg shares its second with the clear.
    #[test]
    fn a_replayed_status_leg_does_not_undo_a_newer_waiting_answer() {
        let first = UnixSecs::now();
        let mut tasks = working_task_we_opened(Instant::now());
        // As the node does after it ingests a leg that surfaces.
        let deliver = |registry: &mut HashMap<TaskId, TaskRecord>, frame| {
            ingest_inbound(registry, frame);
            super::note_surfaced(registry, &tid(), frame);
        };
        let waiting = waiting_leg(Some("asks its user"), first);
        let clear_same_second = waiting_leg(None, first);
        let clear = waiting_leg(None, first + Duration::from_secs(10));
        let waiting_again = waiting_leg(Some("asks again"), first + Duration::from_secs(20));

        deliver(&mut tasks, &waiting);
        deliver(&mut tasks, &clear_same_second);
        assert_eq!(tasks[&tid()].waiting, None);
        ingest_inbound(&mut tasks, &waiting);
        assert_eq!(
            tasks[&tid()].waiting,
            None,
            "a replayed --waiting leg of the same second must not set the reason again"
        );

        deliver(&mut tasks, &clear);
        ingest_inbound(&mut tasks, &waiting);
        assert_eq!(
            tasks[&tid()].waiting,
            None,
            "a replayed --waiting leg must not set the reason again"
        );

        deliver(&mut tasks, &waiting_again);
        assert_eq!(tasks[&tid()].waiting.as_deref(), Some("asks again"));
        ingest_inbound(&mut tasks, &clear);
        assert_eq!(
            tasks[&tid()].waiting.as_deref(),
            Some("asks again"),
            "a replayed older plain leg must not clear the reason"
        );
    }

    /// An MCP reader fetches with an explicit `after`, long or not. Its parked
    /// fetch is a read: a plain poll long ago must not make a reader that has
    /// just fetched look stalled.
    #[tokio::test]
    async fn an_explicit_after_long_fetch_keeps_a_reader_from_stalling() {
        use crate::a2a::surfaced::{PollOrRegisterParams, PollResponder, SurfacedState};

        let mut surfaced = SurfacedState::new();
        let plain_at = tokio::time::Instant::now();
        let (plain_tx, _plain_rx) = tokio::sync::oneshot::channel::<String>();
        surfaced.poll_or_register(PollOrRegisterParams {
            after: None,
            long: false,
            now: plain_at,
            responder: PollResponder::Json(plain_tx),
        });
        let fetch_at = plain_at + STALL_CHECK;
        let (mcp_tx, _mcp_rx) = tokio::sync::oneshot::channel::<String>();
        surfaced.poll_or_register(PollOrRegisterParams {
            after: Some(0),
            long: true,
            now: fetch_at,
            responder: PollResponder::Json(mcp_tx),
        });
        let reader = ReaderBeat {
            bell: surfaced.has_parked_bell(),
            last_poll_secs: surfaced
                .last_poll_secs_ago(fetch_at)
                .expect("the plain poll set a first poll time"),
        };

        let mut tasks = working_task_we_opened(Instant::now());
        assert_eq!(
            notice_for(&mut tasks, &beat_from_worker(Some(reader)), Instant::now()),
            None,
            "a reader that just made an explicit-after fetch is not stalled"
        );
    }

    /// `ingest` guards the same-second replay itself: a caller that does not
    /// call `note_surfaced` (only the node's task path does) gets it too.
    #[test]
    fn ingest_alone_ignores_a_same_second_waiting_replay() {
        let at = UnixSecs::now();
        let mut tasks = working_task_we_opened(Instant::now());
        let waiting = waiting_leg(Some("asks its user"), at);
        let clear = waiting_leg(None, at);

        ingest_inbound(&mut tasks, &waiting);
        assert_eq!(tasks[&tid()].waiting.as_deref(), Some("asks its user"));
        ingest_inbound(&mut tasks, &clear);
        assert_eq!(tasks[&tid()].waiting, None);

        ingest_inbound(&mut tasks, &waiting);
        assert_eq!(
            tasks[&tid()].waiting,
            None,
            "the replayed --waiting leg must not set the reason again"
        );
    }

    /// A working status leg for `tid()` at wire time `at`, with the worker's
    /// order value and a waiting reason when one is given.
    fn ordered_leg(
        reason: Option<&str>,
        at: UnixSecs,
        order_ms: u64,
    ) -> habilis_network::protocol::Message {
        let mut metadata = serde_json::json!({ "mesh:order_ms": order_ms });
        if let Some(reason) = reason {
            metadata["mesh:waiting"] = reason.into();
        }
        let mut frame = status_frame(TaskState::Working, Some(metadata));
        frame.timestamp = at;
        frame
    }

    /// Two legs of one second: the clear is sent after the waiting leg, but
    /// arrives first. The order value keeps the older waiting leg from
    /// restoring its reason.
    #[test]
    fn a_waiting_leg_sent_before_a_same_second_clear_does_not_restore_it() {
        let at = UnixSecs::now();
        let mut tasks = working_task_we_opened(Instant::now());
        let waiting = ordered_leg(Some("asks its user"), at, 1_000);
        let clear = ordered_leg(None, at, 2_000);

        ingest_inbound(&mut tasks, &clear);
        ingest_inbound(&mut tasks, &waiting);
        assert_eq!(
            tasks[&tid()].waiting,
            None,
            "the older waiting leg must not restore its reason after the clear"
        );
    }

    /// A leg without an order value, from a peer that does not send one, is
    /// ordered by its wire second, as before.
    #[test]
    fn a_leg_without_an_order_value_is_ordered_by_its_second() {
        let at = UnixSecs::now();
        let mut tasks = working_task_we_opened(Instant::now());
        ingest_inbound(&mut tasks, &ordered_leg(None, at, 2_000));

        ingest_inbound(
            &mut tasks,
            &waiting_leg(Some("asks its user"), at + Duration::from_secs(1)),
        );
        assert_eq!(tasks[&tid()].waiting.as_deref(), Some("asks its user"));
        ingest_inbound(&mut tasks, &waiting_leg(None, at));
        assert_eq!(
            tasks[&tid()].waiting.as_deref(),
            Some("asks its user"),
            "an older second must not clear the reason"
        );
    }

    /// The clock steps back between two legs: the clear carries a higher order
    /// value but an older wire second. The order value decides, so the clear wins.
    #[test]
    fn a_higher_order_value_wins_over_an_older_wire_second() {
        let at = UnixSecs::now();
        let mut tasks = working_task_we_opened(Instant::now());
        ingest_inbound(
            &mut tasks,
            &ordered_leg(Some("asks its user"), at + Duration::from_secs(1), 1_000),
        );
        assert_eq!(tasks[&tid()].waiting.as_deref(), Some("asks its user"));

        ingest_inbound(&mut tasks, &ordered_leg(None, at, 2_000));
        assert_eq!(
            tasks[&tid()].waiting,
            None,
            "the clear with the higher order value must win"
        );
    }

    /// A reason set by a leg without an order value is ordered by its second
    /// against a later leg that does carry one.
    #[test]
    fn an_ordered_leg_falls_back_to_the_second_after_an_unordered_waiting_leg() {
        let at = UnixSecs::now();
        let mut tasks = working_task_we_opened(Instant::now());
        ingest_inbound(
            &mut tasks,
            &waiting_leg(Some("asks its user"), at + Duration::from_secs(1)),
        );

        ingest_inbound(&mut tasks, &ordered_leg(None, at, 5_000));
        assert_eq!(
            tasks[&tid()].waiting.as_deref(),
            Some("asks its user"),
            "an older second with an order value must not clear a reason set without one"
        );
        ingest_inbound(
            &mut tasks,
            &ordered_leg(None, at + Duration::from_secs(2), 6_000),
        );
        assert_eq!(tasks[&tid()].waiting, None);
    }

    /// The contract: only a `working` leg carries the reason. A leg for any
    /// other live state clears it, even when it carries `mesh:waiting`.
    #[test]
    fn only_a_working_leg_sets_the_waiting_reason() {
        let first = UnixSecs::now();
        let mut tasks = working_task_we_opened(Instant::now());
        let metadata = || crate::a2a::gossip::waiting_metadata(Some("asks its user"));
        let mut leg = status_frame(TaskState::InputRequired, metadata());
        leg.timestamp = first;
        ingest_inbound(&mut tasks, &leg);
        assert_eq!(tasks[&tid()].state, TaskState::InputRequired);
        assert_eq!(
            tasks[&tid()].waiting,
            None,
            "an input-required leg must not set the reason"
        );
    }

    /// A finished task is frozen: a late `--waiting` leg sets nothing.
    #[test]
    fn a_late_waiting_leg_on_a_finished_task_sets_nothing() {
        let mut tasks = working_task_we_opened(Instant::now());
        ingest_inbound(&mut tasks, &status_frame(TaskState::Completed, None));
        assert_eq!(tasks[&tid()].state, TaskState::Completed);

        ingest_inbound(&mut tasks, &waiting_leg(Some("too late"), UnixSecs::now()));
        assert_eq!(tasks[&tid()].waiting, None);
    }

    /// A beat older than one idle timeout proves nothing about the worker now: a
    /// relay can re-inject it after the worker died.
    #[test]
    fn a_stale_beat_raises_no_notice() {
        let now = Instant::now();
        let mut tasks = working_task_we_opened(now);
        let mut beat = beat_from_worker(Some(silent_reader()));
        beat.timestamp = UnixSecs::now() - Duration::from_secs(super::task_timeout_secs() + 1);
        assert_eq!(notice_for(&mut tasks, &beat, now), None);
    }

    /// A working leg with `mesh:waiting` records the reason, and the next
    /// status leg clears it.
    #[test]
    fn a_status_leg_sets_and_clears_the_waiting_reason() {
        let now = Instant::now();
        let mut tasks = working_task_we_opened(now);
        let ingest_leg = |registry: &mut HashMap<TaskId, TaskRecord>, frame| {
            super::ingest(
                registry,
                IngestLegParams {
                    frame: &frame,
                    task_id: &tid(),
                    mine: false,
                    now,
                },
            );
        };

        let waiting = serde_json::json!({ crate::a2a::extensions::META_WAITING: "asks its user" });
        ingest_leg(&mut tasks, status_frame(TaskState::Working, Some(waiting)));
        assert_eq!(tasks[&tid()].waiting.as_deref(), Some("asks its user"));

        ingest_leg(&mut tasks, status_frame(TaskState::Working, None));
        assert_eq!(tasks[&tid()].waiting, None);
    }

    /// A leg on a task that already closed is a replay: anti-entropy served an
    /// old frame after the engine forgot its id. Pre-fix it surfaced again,
    /// so agents saw `completed` twice and stale `working` text after it.
    #[test]
    fn a_leg_on_a_closed_task_is_a_replay() {
        let now = Instant::now();
        let mut tasks = HashMap::new();
        apply(&mut tasks, &leg(LegKind::Offer, true), now);
        tasks.get_mut(&tid()).unwrap().state = TaskState::Completed;
        let late = status_leg(false);
        assert!(super::is_replay(&tasks, &closed(), &tid(), &late.id));
    }

    /// The record goes 2 min after close, and the replays in the field came
    /// 45 min to 4 h late, so the sweep remembers what it reaped.
    #[test]
    fn a_leg_on_a_reaped_task_is_a_replay() {
        let timeout = Duration::from_mins(2);
        let now = Instant::now();
        let mut tasks = HashMap::new();
        apply(&mut tasks, &leg(LegKind::Offer, true), now);
        let rec = tasks.get_mut(&tid()).unwrap();
        rec.state = TaskState::Completed;
        rec.last_activity = now.checked_sub(timeout * 2).unwrap();

        let mut reaped_ids = closed();
        let (_, reaped) = super::sweep_registry(&mut tasks, &mut reaped_ids, now, timeout);
        assert_eq!(reaped, vec![tid()]);
        assert!(tasks.is_empty());
        let late = status_leg(false);
        assert!(super::is_replay(&tasks, &reaped_ids, &tid(), &late.id));
    }

    /// After the sweep reaps a finished task, its own peer can still read it
    /// over gossip. Pre-fix the record was gone and only the id was kept, so a
    /// peer that missed the terminal event got "task not found" and could not
    /// tell a finished task from a lost one. A bystander still gets the refusal.
    #[test]
    fn a_reaped_task_is_still_served_to_its_own_peer() {
        use habilis_network::protocol::MeshId;
        use serde_json::json;

        use crate::a2a::app::A2aApp;
        use crate::a2a::gossip_rpc::{Served, classify};
        use crate::a2a::rpc::A2aOp;

        let timeout = Duration::from_mins(2);
        let now = Instant::now();
        let mut app = A2aApp::new();
        apply(&mut app.tasks, &leg(LegKind::Offer, true), now);
        let rec = app.tasks.get_mut(&tid()).unwrap();
        rec.state = TaskState::Completed;
        rec.last_activity = now.checked_sub(timeout * 2).unwrap();
        super::sweep_registry(&mut app.tasks, &mut app.closed_tasks, now, timeout);
        assert!(app.tasks.is_empty());

        let params = json!({"id": tid().as_str()});
        assert!(
            matches!(
                classify("GetTask", &params, &Nickname::from("calm-otter"), &app),
                Served::Op(A2aOp::GetTask { .. })
            ),
            "the task's own peer must still be served"
        );
        assert!(
            matches!(
                classify("GetTask", &params, &Nickname::from("wire-thistle"), &app),
                Served::Reject(_)
            ),
            "another peer must still get task not found"
        );

        let task = crate::a2a::rpc::get_task(&app, &tid(), &MeshId::from("test")).unwrap();
        assert_eq!(task["id"], tid().as_str());
        assert_eq!(task["status"]["state"], "TASK_STATE_COMPLETED");
        assert_eq!(task["metadata"]["mesh:peer"], "calm-otter");
    }

    /// The closed set stays bounded: past `cap`, the oldest task is forgotten.
    #[test]
    fn closed_tasks_drop_the_oldest_past_the_cap() {
        let mut closed = super::ClosedTasks::new(2);
        let ids = [
            TaskId::from("550e8400-e29b-41d4-a716-446655440001"),
            TaskId::from("550e8400-e29b-41d4-a716-446655440002"),
            TaskId::from("550e8400-e29b-41d4-a716-446655440003"),
        ];
        for id in &ids {
            closed.insert(
                id.clone(),
                super::ClosedTask {
                    peer: Nickname::from("calm-otter"),
                    state: TaskState::Failed,
                },
            );
        }
        assert!(closed.get(&ids[0]).is_none());
        assert!(closed.get(&ids[1]).is_some());
        assert!(closed.get(&ids[2]).is_some());
    }

    #[test]
    fn the_same_leg_surfaces_once() {
        let now = Instant::now();
        let mut tasks = HashMap::new();
        apply(&mut tasks, &leg(LegKind::Offer, true), now);
        let working = status_leg(false);
        super::note_surfaced(&mut tasks, &tid(), &working);
        assert!(super::is_replay(&tasks, &closed(), &tid(), &working.id));
    }

    /// A peer beats every 30 s with a fresh id. Recorded, the beats would push
    /// every real id out of the bounded set in about 32 min, well inside the
    /// replay window.
    #[test]
    fn beats_do_not_evict_surfaced_legs() {
        let now = Instant::now();
        let mut tasks = HashMap::new();
        apply(&mut tasks, &leg(LegKind::Offer, true), now);
        let real = status_leg(false);
        super::note_surfaced(&mut tasks, &tid(), &real);
        for _ in 0..100 {
            super::note_surfaced(&mut tasks, &tid(), &status_leg(true));
        }
        assert!(super::is_replay(&tasks, &closed(), &tid(), &real.id));
    }

    /// Pin: a new leg on a live task surfaces, and so does a leg for a task we
    /// do not know yet (the worker's first leg can beat the RPC response).
    #[test]
    fn a_new_leg_and_an_unknown_task_surface() {
        let now = Instant::now();
        let mut tasks = HashMap::new();
        apply(&mut tasks, &leg(LegKind::Offer, true), now);
        super::note_surfaced(&mut tasks, &tid(), &status_leg(false));
        let fresh = status_leg(false);
        assert!(!super::is_replay(&tasks, &closed(), &tid(), &fresh.id));
        let unknown = TaskId::from("660e8400-e29b-41d4-a716-446655440000");
        assert!(!super::is_replay(&tasks, &closed(), &unknown, &fresh.id));
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
    /// the bystander's text, and a `Beat` refreshed the clock the reaper reads,
    /// so any mesh member could keep an abandoned task alive.
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
        let peer_clock = tasks[&tid()].last_peer_activity;

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
            tasks[&tid()].last_peer_activity,
            peer_clock,
            "a non-party's beat cannot refresh the reaper's clock"
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

    /// The registry a remote peer can mint into is bounded, per peer first.
    ///
    /// Pre-fix there was no cap at all: a `SendMessage` with no taskId skipped
    /// the party check, minted a record and pushed a surfaced event, at
    /// whatever rate the caller managed. The per-peer quota is what keeps a
    /// flooder from locking everyone else out, so this checks that an honest
    /// peer is still admitted while the flooder is refused.
    #[test]
    fn a_peer_cannot_mint_tasks_without_bound() {
        use super::{Admission, admit_new_task};
        let flooder = Nickname::from("wire-thistle");
        let honest = Nickname::from("calm-otter");
        let mut tasks = HashMap::new();
        let now = Instant::now();

        let mut open = |peer: &Nickname, index: usize| {
            let task_id = TaskId::from(format!("550e8400-e29b-41d4-a716-{index:012}").as_str());
            apply(
                &mut tasks,
                &LegInfo {
                    task_id: &task_id,
                    peer,
                    kind: LegKind::Offer,
                    mine: false,
                    fraction: None,
                },
                now,
            );
            task_id
        };

        let mut minted = Vec::new();
        for index in 0..crate::a2a::tuning::TASKS_PER_PEER_CAP {
            minted.push(open(&flooder, index));
        }

        assert_eq!(admit_new_task(&tasks, &flooder), Admission::PeerAtCap);
        assert_eq!(
            admit_new_task(&tasks, &honest),
            Admission::Ok,
            "one peer's flood must not lock another peer out"
        );

        // The quota counts live tasks, so finished ones free it again — an
        // orchestrator running many short tasks with one worker is not a
        // flooder, and must not be treated as one.
        tasks.get_mut(&minted[0]).unwrap().state = TaskState::Completed;
        assert_eq!(admit_new_task(&tasks, &flooder), Admission::Ok);
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
        // A peer that beats, idle well past the debounce.
        let rec = tasks.get_mut(&tid()).unwrap();
        rec.peer_beats = true;
        rec.last_peer_activity = now.checked_sub(timeout * 2).expect("test clock");

        let (expired, reaped) = super::sweep_registry(&mut tasks, &mut closed(), now, timeout);

        assert_eq!(expired.len(), 1, "the idle task is cancelled");
        assert!(reaped.is_empty(), "and is not reaped in the same pass");
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Canceled,
            "the record is retained, frozen terminal, for the dedup window"
        );

        // It reaps once it has actually been terminal for the window.
        let later = now.checked_add(timeout * 2).expect("test clock");
        let (still_live, now_reaped) =
            super::sweep_registry(&mut tasks, &mut closed(), later, timeout);
        assert!(
            still_live.is_empty(),
            "a terminal record is not re-cancelled"
        );
        assert_eq!(now_reaped, vec![tid()]);
        assert!(tasks.is_empty(), "and the registry stays bounded");
    }

    /// The reaper reads the peer's clock, so our own beats cannot hold a task
    /// whose counterparty went silent. Both sides beat since the task
    /// heartbeat, and on the shared `last_activity` a dead worker's task stayed
    /// live for as long as the initiator's daemon kept beating it.
    #[test]
    fn own_beats_do_not_hold_off_the_reaper() {
        let timeout = Duration::from_mins(2);
        let now = Instant::now();
        let long_ago = now.checked_sub(timeout * 2).expect("test clock");
        let mut tasks = HashMap::new();
        apply(&mut tasks, &leg(LegKind::Offer, true), long_ago);
        // Our daemon beat a moment ago; the worker, which beats, has said
        // nothing since.
        let rec = tasks.get_mut(&tid()).unwrap();
        rec.last_activity = now;
        rec.peer_beats = true;

        let (expired, _) = super::sweep_registry(&mut tasks, &mut closed(), now, timeout);
        assert_eq!(
            expired.len(),
            1,
            "a silent peer times out despite our beats"
        );

        // A beat from the peer keeps it live.
        let mut beaten = HashMap::new();
        apply(&mut beaten, &leg(LegKind::Offer, true), long_ago);
        apply(&mut beaten, &leg(LegKind::Beat, false), now);
        let (still_live, _) = super::sweep_registry(&mut beaten, &mut closed(), now, timeout);
        assert!(still_live.is_empty(), "the peer's beat keeps the task live");
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
            tasks[&tid()].last_activity,
            before,
            "nor hold it off the reaper by refreshing its activity clock"
        );
        assert_eq!(
            tasks[&tid()].last_peer_activity,
            before,
            "nor refresh the reaper's own clock"
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
        use habilis_network::protocol::{AppFrameParams, AppTag, MeshId, Message, MessageBody};

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
            IngestLegParams {
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
            msg: Box::new(
                plaintext
                    .sign(&habilis_network::protocol::Identity::generate())
                    .authored(),
            ),
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
    /// its former 120 s cap on skill silence ran out and the timeout ran from there.
    ///
    /// This pins `ingest`, **not** its callers: it calls `ingest` directly. The
    /// caller contract — `broadcast_directed_frame` passing the plaintext twin it
    /// already builds — is pinned at integration level, where a caller regression
    /// is actually observable.
    #[test]
    fn own_status_echo_completes_the_workers_own_record() {
        use habilis_network::protocol::{AppFrameParams, AppTag, MeshId, Message, MessageBody};

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
                IngestLegParams {
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
        rec.last_peer_activity = now.checked_sub(10 * timeout).expect("in range");
        let expired =
            !rec.state.is_terminal() && now.duration_since(rec.last_peer_activity) > timeout;
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

    /// A worker cannot close a task it never started.
    ///
    /// `Rejected` carries a current-state guard; `Completed` had only the
    /// direction one, so a leg straight from `submitted` drove the initiator's
    /// record terminal and frozen with no `working`, no artifact and nothing
    /// delivered — indistinguishable from a real completion, and unrecoverable,
    /// since a terminal record is immutable.
    ///
    /// The decline path is the reason this guard is not simply copied onto
    /// `Failed`: a worker declines a task it never accepted, so `failed` from
    /// `submitted` is legitimate and stays allowed.
    #[test]
    fn a_completed_leg_cannot_close_a_task_that_never_started() {
        let mut tasks = HashMap::new();
        let now = Instant::now();
        apply(&mut tasks, &leg(LegKind::Offer, true), now);
        assert_eq!(tasks[&tid()].state, TaskState::Submitted);

        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Completed), false),
            now,
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Submitted,
            "a task closed from submitted delivered nothing and can never be reopened"
        );

        // The worker's own accept is what opens the door.
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Working), false),
            now,
        );
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Completed), false),
            now,
        );
        assert_eq!(tasks[&tid()].state, TaskState::Completed);

        // Declining an offer outright is a different leg, and still lands.
        let mut declined = HashMap::new();
        apply(&mut declined, &leg(LegKind::Offer, true), now);
        apply(
            &mut declined,
            &leg(LegKind::Status(TaskState::Failed), false),
            now,
        );
        assert_eq!(declined[&tid()].state, TaskState::Failed);
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
