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
use habilis_network::ops::blob::ContentId;
use habilis_network::protocol::messages::Authored;
use habilis_network::protocol::{AuthorKey, Message, MessageId, MessageKind, Nickname, UnixSecs};
use habilis_network::util::bounded_fifo_set::BoundedFifoSet;

use super::status_update_params::Progress;
use super::{META_REASON, TaskId, TaskState, status_update_params, wire};
use crate::a2a::a2a_app::A2aApp;
use crate::a2a::tuning::{
    TASK_LEGS_SEEN_CAP, task_checkin_secs, task_keepalive_secs, task_skill_silence_max_secs,
    task_timeout_secs,
};
use crate::a2a::{broadcast_params, tuning};
use crate::emission;

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
    /// The other party's key. It is the identity: a nickname is a label that two live keys can
    /// share, so a leg is judged by its key.
    pub peer_key: AuthorKey,
    /// The other party's name when the record opened, to show.
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
    /// The check-in clock: the local-clock instant of the last leg an agent sent, on either side.
    /// Beats never touch it, and neither does a `GetTask` snapshot, so it measures how long both
    /// agents have left the task alone. It times the reminder and the alarm, and a forgotten task
    /// stops being beaten.
    pub last_skill_activity: Instant,
    /// Ids of the peer's legs already ingested for this task (at most
    /// `TASK_LEGS_SEEN_CAP`, the oldest dropped first). The engine forgets
    /// a frame id after a bounded count, and a relay can then deliver the same
    /// leg again, so the app remembers what it has seen. Only a first-seen leg
    /// moves the check-in clock. Beats are never recorded: at one every 30 s
    /// they would evict every real id.
    pub legs_seen: BoundedFifoSet<String>,
    /// Last progress reported, replayed on keepalives.
    pub last_progress: Option<Progress>,
    /// Whether the worker waits on its own user, from the `mesh:waiting_user` key of its last
    /// status leg. A label only: it does not move the check-in clock. A check-in without it
    /// clears it.
    pub waiting_user: bool,
    /// The status leg that last set or cleared `waiting_user`. A leg older than this
    /// is a replay and changes nothing. A worker clock that steps back drops
    /// newer waiting updates until it passes this time again, the same exposure
    /// as `is_stale`, accepted.
    pub waiting_mark: Option<WaitingMark>,
    /// What changed, from the last check-in leg, capped. Not replayed.
    pub status_text: Option<String>,
    /// The initiator's one-line name for the task, as the brief carried it.
    pub label: Option<String>,
    /// The ring seq of the sender's follow-up the worker has not answered yet,
    /// so a reminder can name it. Cleared by any leg the worker sends.
    pub followup_seq: Option<u64>,
    /// When this node last raised a reminder (worker) or an alarm (sender), so
    /// one task raises one per interval. Any check-in activity clears it.
    pub last_notice: Option<Instant>,
    /// The last beat of the worker, for the alarm line.
    pub peer_beat: Option<PeerBeat>,
}

/// What the worker's last beat said about its agent, and when it arrived.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PeerBeat {
    pub checkin_age_secs: Option<u64>,
    pub at: Instant,
    /// The wire second the beat was sent, so a beat that a relay delivers late cannot replace a
    /// newer one and shrink the stated age.
    pub sent_at: UnixSecs,
}

impl TaskRecord {
    /// A record of a task that has just begun, in `state`, with every clock at `now`.
    pub(crate) fn open(
        (peer_key, peer): (AuthorKey, Nickname),
        role: TaskRole,
        state: TaskState,
        now: Instant,
    ) -> Self {
        Self {
            peer_key,
            peer,
            role,
            state,
            review: false,
            last_activity: now,
            last_sent: now,
            last_peer_activity: now,
            last_skill_activity: now,
            legs_seen: BoundedFifoSet::new(TASK_LEGS_SEEN_CAP),
            last_progress: None,
            waiting_user: false,
            waiting_mark: None,
            status_text: None,
            label: None,
            followup_seq: None,
            last_notice: None,
            peer_beat: None,
        }
    }

    /// End the task in `state`. Stamping `last_activity` starts the freeze window the GC waits
    /// for, so a late leg is still recognized as a duplicate and not as an unknown task.
    pub(crate) fn close(&mut self, state: TaskState, now: Instant) {
        self.state = state;
        self.review = false;
        self.last_activity = now;
    }

    /// Whoever owes the next move: the worker on a live task, the initiator once the task
    /// parked for its input.
    pub(crate) fn ball(&self) -> TaskRole {
        if self.state == TaskState::InputRequired {
            TaskRole::Initiator
        } else {
            TaskRole::Receiver
        }
    }

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
pub(crate) struct LegInfo<'source> {
    pub task_id: &'source TaskId,
    /// The other party's key (inbound: the author; outbound: the `to`).
    pub peer_key: &'source AuthorKey,
    /// The other party's name. Only an `Offer` reads it, to open the record.
    pub peer: Option<&'source Nickname>,
    pub kind: LegKind,
    /// `true` if we sent this leg (outbound echo), `false` if it arrived.
    pub mine: bool,
    /// The progress a beat carries (else `None`).
    pub progress: Option<Progress>,
    /// Whether this is the first time the leg was seen. A replayed leg changes
    /// the state machine as before but never moves the check-in clock.
    pub first_seen: bool,
}

/// The value cluster [`ingest`] needs beyond its `tasks` registry handle.
#[derive(Clone, Copy)]
pub(crate) struct IngestLegParams<'caller> {
    pub(crate) frame: &'caller Authored,
    pub(crate) task_id: &'caller TaskId,
    pub(crate) mine: bool,
    pub(crate) now: Instant,
}

/// Feed one **validated logical** task frame into the registry from the
/// broadcast (`mine = true`) or receive (`mine = false`) path. The single
/// call site for both: it reduces the frame to a [`LegInfo`] (deriving the
/// peer and the beat progress) and advances the machine. A frame that
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
    let mut checkin = None;
    let mut order = None;
    let mut beat = None;
    let (kind, progress) = match tag.as_str() {
        wire::STATUS => {
            let Ok(payload) = status_update_params::status_payload(frame) else {
                return;
            };
            if status_update_params::is_beat(&payload) {
                beat = Some(PeerBeat {
                    checkin_age_secs: status_update_params::beat_checkin_age(&payload),
                    at: now,
                    sent_at: frame.timestamp,
                });
                (LegKind::Beat, status_update_params::leg_progress(&payload))
            } else {
                if payload.status.state == TaskState::Working {
                    checkin = Some(CheckinFields {
                        waiting_user: status_update_params::status_waiting_user(&payload),
                        text: status_update_params::status_text(&payload),
                        progress: status_update_params::leg_progress(&payload),
                    });
                }
                order = status_update_params::status_order(&payload);
                (LegKind::Status(payload.status.state), None)
            }
        }
        wire::ARTIFACT => (LegKind::Artifact, None),
        _ => return,
    };
    if kind == LegKind::Beat && !mine && is_stale(frame) {
        return;
    }
    let author_key = frame.key();
    let peer_key = if mine { to } else { &author_key };
    // The record's own counterparty is the only authority over it: a bystander's frame must not
    // even be remembered, or it could push a real leg id out of the bounded set.
    if tasks
        .get(task_id)
        .is_some_and(|rec| rec.peer_key != *peer_key)
    {
        return;
    }
    // Remembered on the first ingest of every inbound leg, surfaced or not, so a
    // replay of a silent one cannot move the clock either.
    let first_seen = mine || kind == LegKind::Beat || mark_seen(tasks, task_id, frame.id.as_str());
    // A status leg that a slow relay delivers after a newer one is a first sight of its id, but
    // not news: it must not restart the check-in clock.
    let moves_clock = first_seen
        && (mine
            || !matches!(kind, LegKind::Status(_))
            || tasks
                .get(task_id)
                .is_none_or(|rec| is_newer_than_waiting(rec, frame.timestamp, order)));
    apply(
        tasks,
        &LegInfo {
            task_id,
            peer_key,
            peer: None,
            kind,
            mine,
            progress,
            first_seen: moves_clock,
        },
        now,
    );
    let Some(rec) = tasks.get_mut(task_id) else {
        return;
    };
    // Only a beat from the other party says anything about its reader and age.
    if !mine
        && let Some(beat) = beat
        && rec
            .peer_beat
            .is_none_or(|held| held.sent_at <= beat.sent_at)
    {
        rec.peer_beat = Some(beat);
    }
    // Every working leg restates whom the worker waits on, so a plain one clears
    // it. A relay can replay an old leg, so two checks gate the update:
    // `is_newer_than_waiting` drops a leg older than the last answer, and
    // `first_seen` drops a leg already ingested, which covers a replay in the
    // same second as the answer. `apply` above still runs for every leg and
    // `advance` is idempotent. A finished task waits for nothing.
    if matches!(kind, LegKind::Status(_)) {
        if rec.state.is_terminal() {
            rec.waiting_user = false;
        } else if first_seen && is_newer_than_waiting(rec, frame.timestamp, order) {
            record_checkin(rec, checkin);
            rec.waiting_mark = Some(WaitingMark::new(frame.timestamp, order));
        }
    }
    // A delivered result answers whatever the worker waited on, and no check-in needs to say so.
    if kind == LegKind::Artifact
        && first_seen
        && !rec.state.is_terminal()
        && is_newer_than_waiting(rec, frame.timestamp, None)
    {
        rec.waiting_user = false;
        rec.waiting_mark = Some(WaitingMark::new(frame.timestamp, None));
    }
}

/// Keep what a status leg says about the work. A check-in sets the flag, the
/// text and the progress it carries; any other status leg clears the flag.
fn record_checkin(rec: &mut TaskRecord, checkin: Option<CheckinFields>) {
    let Some(checkin) = checkin else {
        rec.waiting_user = false;
        return;
    };
    rec.waiting_user = checkin.waiting_user;
    if checkin.text.is_some() {
        rec.status_text = checkin.text;
    }
    if checkin.progress.is_some() {
        rec.last_progress = checkin.progress;
    }
}

/// The check-in fields of a `working` status leg.
struct CheckinFields {
    waiting_user: bool,
    text: Option<String>,
    progress: Option<Progress>,
}

/// Whether an inbound check-in puts the progress in another quarter than the
/// one the record holds. The first progress after none does not count, and a
/// beat never does: it only replays the last progress. A beat that arrives before the check-in
/// it replays moves `last_progress` first, and that wake is lost; the cost is accepted.
pub(crate) fn quarter_moved(rec: Option<&TaskRecord>, frame: &Authored) -> bool {
    let Ok(payload) = status_update_params::status_payload(frame) else {
        return false;
    };
    if payload.status.state != TaskState::Working || status_update_params::is_beat(&payload) {
        return false;
    }
    // A late, older check-in changes nothing in the record, so it wakes nobody.
    let order = status_update_params::status_order(&payload);
    if rec.is_some_and(|rec| !is_newer_than_waiting(rec, frame.timestamp, order)) {
        return false;
    }
    let held = rec.and_then(|rec| rec.last_progress).map(Progress::quarter);
    let new = status_update_params::leg_progress(&payload).map(Progress::quarter);
    matches!((held, new), (Some(held), Some(new)) if held != new)
}

/// Remember `leg_id` for the task and say whether it is new. A task with no
/// record yet has nothing to protect, so its first leg counts as new.
pub(crate) fn mark_seen(
    tasks: &mut HashMap<TaskId, TaskRecord>,
    task_id: &TaskId,
    leg_id: &str,
) -> bool {
    let Some(rec) = tasks.get_mut(task_id) else {
        return true;
    };
    if rec.legs_seen.contains(leg_id) {
        return false;
    }
    rec.legs_seen.insert(leg_id.to_owned());
    true
}

/// The seq of the newest `task_message` event in the ring above `after`, once the tap is drained.
/// The ring can end with another event, such as the loss marker, so the newest seq is not
/// necessarily the one a caller just emitted.
pub(crate) fn newest_task_message_seq(app: &mut A2aApp, after: Option<u64>) -> Option<u64> {
    use crate::emission::OutputEvent;

    app.drain_surfaced_ring();
    app.surfaced
        .poll_since(None)
        .events
        .iter()
        .rev()
        .find(|item| {
            matches!(item.event, OutputEvent::TaskMessage { .. })
                && after.is_none_or(|after| item.seq > after)
        })
        .map(|item| item.seq)
}

/// The oldest and the newest seq the surfaced ring holds, once its tap is drained.
pub(crate) fn ring_bounds(app: &mut A2aApp) -> Option<(u64, u64)> {
    app.drain_surfaced_ring();
    let batch = app.surfaced.poll_since(None);
    Some((batch.events.first()?.seq, batch.events.last()?.seq))
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

/// Whether the check-in clock runs: only while the worker holds the ball. A
/// question or an artifact waiting on the sender stops it.
fn clock_runs(rec: &TaskRecord) -> bool {
    matches!(rec.state, TaskState::Submitted | TaskState::Working)
}

/// How long the worker's agent has gone without a check-in: the smaller of the
/// age here (last leg received) and the age in the worker's last beat, so one
/// lost silent check-in raises no false alarm.
fn checkin_age(rec: &TaskRecord, now: Instant) -> Duration {
    let local = now.duration_since(rec.last_skill_activity);
    let stated = rec.peer_beat.and_then(|beat| {
        beat.checkin_age_secs
            .map(|age| Duration::from_secs(age).saturating_add(now.duration_since(beat.at)))
    });
    stated.map_or(local, |stated| stated.min(local))
}

/// A raised alarm: the worker's agent has not checked in for 2·X.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Overdue {
    pub task_id: TaskId,
    pub peer: Nickname,
    pub label: Option<String>,
    pub age_secs: u64,
    pub waiting_user: bool,
    pub not_picked_up: bool,
}

/// A raised reminder: this worker owes a check-in.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CheckinDue {
    pub task_id: TaskId,
    pub peer: Nickname,
    pub followup_seq: Option<u64>,
}

/// The alarms and reminders that fall due at `now`, each marked so the next
/// one waits a full interval. The alarm is the sender's, from 2·X after the
/// last activity; the reminder is the worker's, from X. A worker whose beats
/// carry no check-in age counts the silence it sees itself.
pub(crate) fn due_checkins(
    tasks: &mut HashMap<TaskId, TaskRecord>,
    interval: Duration,
    now: Instant,
) -> (Vec<Overdue>, Vec<CheckinDue>) {
    let mut overdue = Vec::new();
    let mut reminders = Vec::new();
    for (task_id, rec) in tasks.iter_mut() {
        if !clock_runs(rec)
            || rec
                .last_notice
                .is_some_and(|at| now.duration_since(at) < interval)
        {
            continue;
        }
        match rec.role {
            TaskRole::Initiator => {
                let age = checkin_age(rec, now);
                if age >= interval * 2 {
                    rec.last_notice = Some(now);
                    let not_picked_up = rec.state == TaskState::Submitted;
                    let label = rec.label.clone();
                    let waiting_user = rec.waiting_user;
                    overdue.push(Overdue {
                        task_id: task_id.clone(),
                        peer: rec.peer.clone(),
                        label,
                        age_secs: age.as_secs(),
                        waiting_user,
                        not_picked_up,
                    });
                }
            }
            TaskRole::Receiver => {
                if now.duration_since(rec.last_skill_activity) >= interval {
                    rec.last_notice = Some(now);
                    reminders.push(CheckinDue {
                        task_id: task_id.clone(),
                        peer: rec.peer.clone(),
                        followup_seq: rec.followup_seq,
                    });
                }
            }
        }
    }
    (overdue, reminders)
}

/// Raise the alarms and reminders that are due, on the tick.
pub(crate) fn tick_checkins(app: &mut A2aApp) {
    let (overdue, reminders) = due_checkins(
        &mut app.tasks,
        Duration::from_secs(task_checkin_secs()),
        Instant::now(),
    );
    for notice in &overdue {
        app.output.task_overdue(notice);
    }
    // The ring is bounded, so the follow-up a reminder names may have aged out.
    let oldest = if reminders.is_empty() {
        None
    } else {
        ring_bounds(app).map(|(oldest, _)| oldest)
    };
    for notice in &reminders {
        let aged_out = notice
            .followup_seq
            .is_some_and(|seq| oldest.is_none_or(|oldest| seq < oldest));
        app.output.checkin_due(notice, aged_out);
    }
}

/// What a reaped task keeps: enough to answer its own peer's `GetTask`, not
/// the full record.
pub(crate) struct ClosedTask {
    pub peer_key: AuthorKey,
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
/// this leg id was ingested before. An unknown task still surfaces: the
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
    let Some(rec) = tasks.get(task_id) else {
        return false;
    };
    rec.state.is_terminal() || rec.legs_seen.contains(leg_id.as_str())
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
pub(crate) fn admit_new_task(
    tasks: &HashMap<TaskId, TaskRecord>,
    peer_key: &AuthorKey,
) -> Admission {
    if tasks.len() >= tuning::TASKS_CAP {
        return Admission::RegistryFull;
    }
    let live_with_peer = tasks
        .values()
        .filter(|rec| rec.peer_key == *peer_key && !rec.state.is_terminal())
        .count();
    if live_with_peer >= tuning::TASKS_PER_PEER_CAP {
        return Admission::PeerAtCap;
    }
    Admission::Ok
}

/// The value cluster [`adopt_initiator`] needs beyond its `tasks` registry
/// handle.
#[derive(Clone, Copy)]
pub(crate) struct AdoptInitiatorParams<'caller> {
    pub(crate) task_id: &'caller TaskId,
    pub(crate) peer_key: &'caller AuthorKey,
    pub(crate) peer: &'caller Nickname,
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
        peer_key,
        peer,
        task_state,
        now,
    } = params;
    // A refused adoption leaves no record here; the worker still holds its own.
    if !tasks.contains_key(task_id) && admit_new_task(tasks, peer_key) != Admission::Ok {
        return;
    }
    match tasks.entry(task_id.clone()) {
        Entry::Vacant(slot) => {
            // First adoption (task creation): open the initiator-side record at
            // the server-minted state.
            slot.insert(TaskRecord::open(
                (*peer_key, peer.clone()),
                TaskRole::Initiator,
                task_state,
                now,
            ));
        }
        Entry::Occupied(mut slot) => {
            // Already tracking. The worker-pushed status/artifact stream is the
            // authoritative source of *live* transitions, so a late or reordered
            // RPC snapshot (e.g. a `GetTask` reply that raced the artifact push)
            // must NOT regress a live non-terminal state. But a **terminal**
            // snapshot is final truth — it repairs a terminal frame we missed
            // (a frame that was lost and that nothing sends again), the reconciliation path a
            // manual `GetTask` gives.
            let rec = slot.get_mut();
            // …and only from the task's own worker. The caller has already
            // proved this response answers a call we made to `peer`, but that
            // says nothing about *which* task the answer may name: any peer we
            // call once could otherwise return a Task id belonging to a
            // different peer and drive it terminal, or keep refreshing it past
            // the reaper. The record's own counterparty is the authority.
            if rec.peer_key != *peer_key {
                return;
            }
            if rec.state.is_terminal() {
                return;
            }
            rec.last_activity = now;
            rec.last_peer_activity = now;
            if task_state.is_terminal() {
                rec.close(task_state, now);
            }
        }
    }
}

/// Record a follow-up we just sent into a task we opened. It hands the task back to the worker
/// at dispatch, not on the reply, so a lost reply or request leaves no record parked on us with
/// the check-in clock stopped.
pub(crate) fn note_follow_up_sent(
    tasks: &mut HashMap<TaskId, TaskRecord>,
    task_id: &TaskId,
    peer_key: &AuthorKey,
    now: Instant,
) {
    if tasks
        .get(task_id)
        .is_none_or(|rec| rec.role != TaskRole::Initiator)
    {
        return;
    }
    apply(
        tasks,
        &LegInfo {
            task_id,
            peer_key,
            peer: None,
            kind: LegKind::Text,
            mine: true,
            progress: None,
            first_seen: true,
        },
        now,
    );
}

/// Give the initiator-side record the task label its worker echoed in `GetTask`,
/// unless it already holds one. A snapshot from another peer changes nothing.
pub(crate) fn set_label(
    tasks: &mut HashMap<TaskId, TaskRecord>,
    task_id: &TaskId,
    peer_key: &AuthorKey,
    label: Option<String>,
) {
    if let Some(rec) = tasks.get_mut(task_id)
        && rec.role == TaskRole::Initiator
        && rec.peer_key == *peer_key
        && rec.label.is_none()
    {
        rec.label = label;
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

    if matches!(leg.kind, LegKind::Offer)
        && let Some(peer) = leg.peer
    {
        // The opening leg mints the record; a duplicate offer just touches it.
        tasks.entry(leg.task_id.clone()).or_insert_with(|| {
            TaskRecord::open(
                (*leg.peer_key, peer.clone()),
                TaskRole::Receiver,
                TaskState::Submitted,
                now,
            )
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
    if rec.peer_key != *leg.peer_key {
        return;
    }

    advance(rec, leg.kind, leg.mine);
    if let Some(progress) = leg.progress {
        rec.last_progress = Some(progress);
    }
    rec.last_activity = now;
    if leg.mine {
        rec.last_sent = now;
    } else {
        rec.last_peer_activity = now;
    }
    if leg.kind != LegKind::Beat && leg.first_seen {
        rec.last_skill_activity = now;
        rec.last_notice = None;
        if leg.mine && rec.role == TaskRole::Receiver {
            rec.followup_seq = None;
        }
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
        }
        LegKind::Status(TaskState::InputRequired)
            if sender == TaskRole::Receiver
                && matches!(rec.state, TaskState::Submitted | TaskState::Working) =>
        {
            // The worker asks a question → the initiator owes the answer.
            rec.state = TaskState::InputRequired;
            rec.review = false;
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

/// Tell each worker in `cancels` that its task is cancelled, and why.
async fn send_cancels(
    state: &EventLoopState,
    context: &HandlerCtx<'_>,
    cancels: Vec<(&TaskId, &AuthorKey, &str)>,
) {
    for (task_id, peer_key, reason) in cancels {
        let update = status_update_params::status_update(
            context.mesh,
            status_update_params::StatusUpdateParams::new(task_id, TaskState::Canceled)
                .with_metadata(serde_json::json!({ META_REASON: reason })),
        );
        broadcast_status(
            state,
            context,
            BroadcastStatusParams {
                peer_key,
                update: &update,
            },
        )
        .await;
    }
}

/// Send the cancels that the app owes now, without waiting for the task sweep. The receive path
/// calls it when the list reached its cap, so a cancel is never dropped for want of room.
pub(crate) async fn send_late_cancels(
    state: &EventLoopState,
    app: &mut A2aApp,
    context: &HandlerCtx<'_>,
) {
    let owed = app.take_late_cancels();
    let cancels: Vec<(&TaskId, &AuthorKey, &str)> = owed
        .iter()
        .map(|(task_id, peer_key, reason)| (task_id, peer_key, *reason))
        .collect();
    send_cancels(state, context, cancels).await;
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
    context: &HandlerCtx<'_>,
    out: &emission::Output,
) {
    let now = Instant::now();
    let timeout = Duration::from_secs(task_timeout_secs());
    let (expired, reaped) = sweep_registry(&mut app.tasks, &mut app.closed_tasks, now, timeout);

    for (task_id, peer_key) in &expired {
        out.task_timeout(task_id, emission::TaskGoneReason::Timeout);
        tracing::debug!(%task_id, peer = %peer_key, "task evicted (idle-debounce timeout)");
    }
    // A task a late reply opened is unknown to the caller's agent, so it gets
    // no `task_timeout` line — only the worker is told.
    let late_cancels = app.take_late_cancels();
    let mut cancels: Vec<(&TaskId, &AuthorKey, &str)> = expired
        .iter()
        .map(|(task_id, peer_key)| (task_id, peer_key, "timeout"))
        .collect();
    let late: Vec<(&TaskId, &AuthorKey, &str)> = late_cancels
        .iter()
        .map(|(task_id, peer_key, reason)| (task_id, peer_key, *reason))
        .collect();
    cancels.extend(late);
    send_cancels(state, context, cancels).await;

    // A reaped task's offloaded blobs go with it — unlink their spool files
    // (the review window has long closed).
    let call_blobs = app.take_call_blobs(&reaped, now);
    if let Some(server) = app.blob_server.as_ref() {
        for task_id in &reaped {
            server
                .evict_content(&ContentId::new(task_id.as_str()))
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
) -> (Vec<(TaskId, AuthorKey)>, Vec<TaskId>) {
    let mut expired = Vec::new();
    for (task_id, rec) in tasks.iter_mut() {
        let peer_silent = now.duration_since(rec.last_peer_activity) > timeout;
        if !rec.state.is_terminal() && peer_silent {
            rec.close(TaskState::Canceled, now);
            expired.push((task_id.clone(), rec.peer_key));
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
                    peer_key: rec.peer_key,
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
    peer_key: &AuthorKey,
    out: &emission::Output,
) {
    let now = Instant::now();
    for (task_id, rec) in tasks.iter_mut() {
        if rec.peer_key == *peer_key && !rec.state.is_terminal() {
            rec.close(TaskState::Canceled, now);
            out.task_timeout(task_id, emission::TaskGoneReason::PeerLeft);
            tracing::debug!(%task_id, peer = %peer_key, "task canceled (peer left)");
        }
    }
}

/// Emit a beat (keepalive) status for every live task we have gone quiet on
/// past the keepalive cadence. The task analogue of the engine's own lifecycle
/// keepalive tick. See [`TaskRecord::should_keepalive`].
pub(crate) async fn tick_task_keepalive(
    state: &mut EventLoopState,
    app: &mut A2aApp,
    context: &HandlerCtx<'_>,
) {
    /// `(task_id, peer, state, last_progress, check-in age)` for one keepalive-due task.
    type KeepaliveDue = (TaskId, AuthorKey, TaskState, Option<Progress>, Option<u64>);

    let now = Instant::now();
    let cadence = Duration::from_secs(task_keepalive_secs());
    let quiet = app
        .tasks
        .iter()
        .filter(|(_, rec)| rec.should_keepalive(now, cadence));
    let due: Vec<KeepaliveDue> = quiet
        .map(|(task_id, rec)| {
            // Only the worker states an age: the sender's silence is not a check-in.
            let age = (rec.role == TaskRole::Receiver)
                .then(|| now.duration_since(rec.last_skill_activity).as_secs());
            (
                task_id.clone(),
                rec.peer_key,
                rec.state,
                rec.last_progress,
                age,
            )
        })
        .collect();

    for (task_id, peer_key, task_state, progress, checkin_age_secs) in due {
        let update = status_update_params::status_update(
            context.mesh,
            status_update_params::StatusUpdateParams::new(&task_id, task_state).with_metadata(
                status_update_params::beat_metadata_with_extras(status_update_params::BeatExtras {
                    progress,
                    checkin_age_secs,
                }),
            ),
        );
        let sent = broadcast_status(
            state,
            context,
            BroadcastStatusParams {
                peer_key: &peer_key,
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
struct BroadcastStatusParams<'caller> {
    peer_key: &'caller AuthorKey,
    update: &'caller super::TaskStatusUpdate,
}

/// Build, sign, and fire-and-forget a daemon-originated status frame (the
/// keepalive beat and the timeout cancel). A serialize error is swallowed
/// like any other plumbing broadcast — the payloads are small literals.
/// Returns whether the send started, so a beat that could not start is not
/// counted as sent and retries on the next tick instead of a full cadence
/// later. A send that starts and then fails costs one cadence.
async fn broadcast_status(
    state: &EventLoopState,
    context: &HandlerCtx<'_>,
    params: BroadcastStatusParams<'_>,
) -> bool {
    let BroadcastStatusParams { peer_key, update } = params;
    let Ok(body) = status_update_params::payload_body(update) else {
        return false;
    };
    // Directed status frames are sealed to the peer like every other directed
    // frame (the receive path always unseals a directed body). If the peer's key
    // isn't known yet, skip this beat/cancel — it is fire-and-forget plumbing and
    // a later one retries.
    let Ok(body) = broadcast_params::seal_directed(state, peer_key, &body) else {
        return false;
    };
    let kind = MessageKind::App {
        tag: wire::app_tag(wire::STATUS),
        to: Some(*peer_key),
        corr: None,
    };
    let message =
        Message::new_frame(context.mesh, context.author, kind, body).sign(state.identity());
    let Ok(bytes) = message.serialize() else {
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
    habilis_network::ops::deliver_in_background(&message, bytes, state, context.sender).await
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};
    use std::time::{Duration, Instant};

    use habilis_network::protocol::messages::Authored;
    use habilis_network::protocol::{AuthorKey, Identity, Message, Nickname, UnixSecs};

    use super::{IngestLegParams, LegInfo, LegKind, TaskRecord, TaskRole, TaskState, apply};
    use crate::a2a::status_update_params::Progress;
    use crate::a2a::tuning::{TASK_LEGS_SEEN_CAP, TASK_STATUS_TEXT_MAX_CHARS};
    use crate::a2a::{
        TaskId, extensions, rpc_error, status_update_params, task_record, tuning, wire,
    };

    fn tid() -> TaskId {
        TaskId::from("550e8400-e29b-41d4-a716-446655440000")
    }

    /// The identity of the test peer called `name`. A name is a label: the identity is the peer.
    fn identity_of(name: &str) -> &'static Identity {
        static IDENTITIES: LazyLock<Mutex<HashMap<String, &'static Identity>>> =
            LazyLock::new(|| Mutex::new(HashMap::new()));
        let mut identities = IDENTITIES
            .lock()
            .expect("a panic in another test poisons this table, and that test fails first");
        identities
            .entry(name.to_owned())
            .or_insert_with(|| Box::leak(Box::new(Identity::generate())))
    }

    fn peer_key(name: &str) -> AuthorKey {
        identity_of(name).author_key()
    }

    /// `frame` as the receiver of the peer called `name` sees it: signed by that peer.
    fn authored_by(name: &str, frame: &Message) -> Authored {
        frame.clone().sign(identity_of(name)).authored()
    }

    /// Every live task is beaten once quiet past the cadence — on either side,
    /// whoever holds the ball, however long the skill has been silent. Pre-fix
    /// only the ball-owner beat, and only within 120 s of its skill's last leg,
    /// so an accept question, a review, or a long build lost the task.
    #[test]
    fn keepalive_covers_every_live_task_past_the_cadence() {
        let cadence = Duration::from_secs(30);
        let now = Instant::now();
        let quiet = now
            .checked_sub(2 * cadence)
            .expect("the test instant is far from the clock origin");

        // I initiated it and the worker holds the ball.
        let mut tasks = HashMap::new();
        open_as_initiator(&mut tasks, quiet);
        let rec = tasks
            .get_mut(&tid())
            .expect("the test inserted this task earlier");
        assert_eq!(
            rec.ball(),
            TaskRole::Receiver,
            "the ball passes to the receiver"
        );
        assert!(
            rec.should_keepalive(now, cadence),
            "the side without the ball beats too"
        );

        // Silent for an hour: still beaten.
        let hour_ago = now
            .checked_sub(Duration::from_hours(1))
            .expect("the test instant is far from the clock origin");
        rec.last_sent = hour_ago;
        assert!(
            rec.should_keepalive(now, cadence),
            "skill silence does not stop the beat"
        );

        // Within the cadence ⇒ not due yet.
        rec.last_sent = now;
        assert!(
            !rec.should_keepalive(now, cadence),
            "a finished task is not kept alive"
        );

        // A terminal task is never beaten.
        rec.last_sent = quiet;
        rec.state = TaskState::Canceled;
        assert!(
            !rec.should_keepalive(now, cadence),
            "a stalled task is not kept alive"
        );
    }

    /// The peer's beat must not stand in for ours. The cadence once read the
    /// clock that inbound legs refresh too, so whichever side beat first kept
    /// the other side quiet, and that side's peer then reaped the task.
    #[test]
    fn a_peer_beat_does_not_suppress_our_beat() {
        let cadence = Duration::from_secs(30);
        let now = Instant::now();
        let mut tasks = HashMap::new();
        open_as_initiator(
            &mut tasks,
            now.checked_sub(2 * cadence)
                .expect("the test instant is far from the clock origin"),
        );
        apply(&mut tasks, &leg(LegKind::Beat, false), now);
        assert!(
            tasks[&tid()].should_keepalive(now, cadence),
            "a working task is kept alive at the cadence"
        );
    }

    /// A task no agent has touched for the silence cap stops being beaten, so
    /// the peer reaps it. Without the cap, a task forgotten after a `/clear`
    /// was beaten for as long as both daemons ran, and 64 of them filled the
    /// per-peer quota.
    #[test]
    fn a_forgotten_task_stops_beating_after_the_silence_cap() {
        let cadence = Duration::from_secs(30);
        let now = Instant::now();
        let day_ago = now
            .checked_sub(Duration::from_hours(25))
            .expect("the test instant is far from the clock origin");
        let mut tasks = HashMap::new();
        open_as_initiator(&mut tasks, day_ago);
        // Only the daemons have spoken since: the peer's beat, and ours.
        apply(&mut tasks, &leg(LegKind::Beat, false), now);
        tasks
            .get_mut(&tid())
            .expect("the test inserted this task earlier")
            .last_sent = day_ago;
        assert!(
            !tasks[&tid()].should_keepalive(now, cadence),
            "a quiet task past the cadence is not kept alive"
        );
    }

    /// The silence cap still ends a task that nobody has touched for a day.
    #[test]
    fn a_task_left_alone_past_the_silence_cap_is_reaped() {
        let timeout = Duration::from_mins(2);
        let now = Instant::now();
        let day_ago = now
            .checked_sub(Duration::from_hours(25))
            .expect("the test instant is far from the clock origin");
        let mut tasks = HashMap::new();
        apply(&mut tasks, &leg(LegKind::Offer, false), day_ago);

        let (expired, _) = super::sweep_registry(&mut tasks, &mut closed(), now, timeout);
        assert_eq!(expired.len(), 1, "the one expired task is reported");
    }

    /// A beat is only evidence while it is fresh. The engine dedups by a
    /// bounded count, not by age, so a relay that re-injects an old beat after
    /// the peer died must not keep the task alive.
    #[test]
    fn a_stale_beat_does_not_refresh_the_peer_clock() {
        use habilis_network::protocol::{AppFrameParams, MeshId, Message, MessageBody};

        let mesh = MeshId::from("test");
        let now = Instant::now();
        let mut tasks = HashMap::new();
        open_as_initiator(&mut tasks, now);
        let peer_clock = tasks[&tid()].last_peer_activity;

        let payload = status_update_params::status_update(
            &mesh,
            status_update_params::StatusUpdateParams::new(&tid(), TaskState::Working)
                .with_metadata(status_update_params::beat_metadata(None)),
        );
        let mut beat = Message::new_app(
            &mesh,
            &Nickname::from("calm-otter"),
            AppFrameParams::new(
                wire::app_tag(wire::STATUS),
                Some(peer_key("worker-bot")),
                None,
                MessageBody::try_new(
                    &serde_json::to_string(&payload).expect("a test value serializes to JSON"),
                )
                .expect("a test value serializes to JSON"),
            ),
        );
        beat.timestamp = beat.timestamp - Duration::from_hours(1);

        let later = now
            .checked_add(Duration::from_mins(1))
            .expect("the test instant cannot overflow");
        super::ingest(
            &mut tasks,
            IngestLegParams {
                frame: &authored_by("calm-otter", &beat),
                task_id: &tid(),
                mine: false,
                now: later,
            },
        );
        assert_eq!(
            tasks[&tid()].last_peer_activity,
            peer_clock,
            "a stale beat is a replayed frame, so it must not make the peer look alive again"
        );
    }

    fn closed() -> super::ClosedTasks {
        super::ClosedTasks::new(8)
    }

    /// An inbound status frame for `tid()`; a beat when `beat` is true.
    fn status_leg(beat: bool) -> Message {
        status_frame(
            TaskState::Working,
            beat.then(|| status_update_params::beat_metadata(None)),
        )
    }

    /// An inbound status frame for `tid()` in `state`, carrying `metadata`.
    fn status_frame(state: TaskState, metadata: Option<serde_json::Value>) -> Message {
        use habilis_network::protocol::{AppFrameParams, MeshId, Message, MessageBody};

        let mesh = MeshId::from("test");
        let payload = status_update_params::status_update(
            &mesh,
            status_update_params::StatusUpdateParams {
                task_id: &tid(),
                state,
                note: None,
                metadata,
            },
        );
        Message::new_app(
            &mesh,
            &Nickname::from("calm-otter"),
            AppFrameParams::new(
                wire::app_tag(wire::STATUS),
                Some(peer_key("worker-bot")),
                None,
                MessageBody::try_new(
                    &serde_json::to_string(&payload).expect("a test value serializes to JSON"),
                )
                .expect("a test value serializes to JSON"),
            ),
        )
    }

    /// A task we opened (initiator) whose worker reported `working`.
    fn working_task_we_opened(now: Instant) -> HashMap<TaskId, TaskRecord> {
        let mut tasks = HashMap::new();
        open_as_initiator(&mut tasks, now);
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Working), false),
            now,
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Working,
            "the task is working after the leg"
        );
        tasks
    }

    /// The check-in interval of the tests, X.
    const CHECKIN: Duration = Duration::from_mins(5);
    const JUST_BEFORE_X: Duration = Duration::from_secs(299);
    const JUST_BEFORE_TWO_X: Duration = Duration::from_secs(599);

    /// A check-in leg from the worker: the note, a percentage, and whom it waits on.
    fn checkin_leg(text: &str, percent: &str, waiting_user: bool) -> Message {
        use habilis_network::protocol::{AppFrameParams, MeshId, MessageBody};

        let mesh = MeshId::from("test");
        let progress: Progress = percent.parse().expect("the test passes a valid percentage");
        let payload = status_update_params::status_update(
            &mesh,
            status_update_params::StatusUpdateParams {
                task_id: &tid(),
                state: TaskState::Working,
                note: Some(text),
                metadata: Some(status_update_params::status_metadata(
                    waiting_user,
                    Some(progress),
                    status_update_params::leg_order_ms(),
                )),
            },
        );
        Message::new_app(
            &mesh,
            &Nickname::from("calm-otter"),
            AppFrameParams::new(
                wire::app_tag(wire::STATUS),
                Some(peer_key("worker-bot")),
                None,
                MessageBody::try_new(
                    &serde_json::to_string(&payload).expect("a test value serializes to JSON"),
                )
                .expect("a test value serializes to JSON"),
            ),
        )
    }

    /// A worker's record of a task it serves: offered by the peer at `at`.
    fn task_we_serve(at: Instant) -> HashMap<TaskId, TaskRecord> {
        let mut tasks = HashMap::new();
        apply(&mut tasks, &leg(LegKind::Offer, false), at);
        tasks
    }

    fn progress_of(percent: u8) -> Progress {
        Progress::try_from(percent).expect("the test passes a percentage")
    }

    fn later(at: Instant, by: Duration) -> Instant {
        at.checked_add(by)
            .expect("the test instant cannot overflow")
    }

    /// The reminder is due X after the last activity, then every X.
    #[test]
    fn the_reminder_fires_at_x_and_then_every_x() {
        let start = Instant::now();
        let mut tasks = task_we_serve(start);
        let due = |registry: &mut HashMap<TaskId, TaskRecord>, after: Duration| {
            super::due_checkins(registry, CHECKIN, later(start, after))
                .1
                .len()
        };
        assert_eq!(due(&mut tasks, JUST_BEFORE_X), 0, "nothing is due before X");
        assert_eq!(
            due(&mut tasks, CHECKIN),
            1,
            "X after the last activity the worker owes a check-in"
        );
        assert_eq!(
            due(&mut tasks, CHECKIN + Duration::from_secs(1)),
            0,
            "one reminder per interval"
        );
        assert_eq!(due(&mut tasks, CHECKIN * 2), 1, "then again every X");
    }

    /// The alarm is due 2·X after the last activity, then every X.
    #[test]
    fn the_alarm_fires_at_two_x_and_then_every_x() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        let overdue = |registry: &mut HashMap<TaskId, TaskRecord>, after: Duration| {
            super::due_checkins(registry, CHECKIN, later(start, after))
                .0
                .len()
        };
        assert_eq!(
            overdue(&mut tasks, CHECKIN),
            0,
            "X is a reminder, not an alarm"
        );
        assert_eq!(
            overdue(&mut tasks, JUST_BEFORE_TWO_X),
            0,
            "nothing is overdue before 2·X"
        );
        assert_eq!(
            overdue(&mut tasks, CHECKIN * 2),
            1,
            "the alarm is due at 2·X"
        );
        assert_eq!(
            overdue(&mut tasks, CHECKIN * 2 + Duration::from_secs(1)),
            0,
            "one alarm per interval"
        );
        assert_eq!(overdue(&mut tasks, CHECKIN * 3), 1, "then again every X");
    }

    /// A beat carries the worker's check-in age for the alarm line, and proves only that the
    /// daemon lives.
    #[test]
    fn a_beat_with_a_check_in_age_does_not_restart_the_clock() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        let beat = status_frame(
            TaskState::Working,
            Some(status_update_params::beat_metadata_with_extras(
                status_update_params::BeatExtras {
                    checkin_age_secs: Some(30),
                    ..Default::default()
                },
            )),
        );
        ingest_inbound(&mut tasks, &beat);
        assert_eq!(
            tasks[&tid()].last_skill_activity,
            start,
            "a beat proves the daemon lives, not the agent"
        );
    }

    /// The smaller of the two ages counts, so one lost silent check-in raises no alarm.
    #[test]
    fn the_smaller_age_wins() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        let at = later(start, CHECKIN * 3);
        tasks
            .get_mut(&tid())
            .expect("the test inserted this task earlier")
            .peer_beat = Some(super::PeerBeat {
            checkin_age_secs: Some(10),
            at,
            sent_at: UnixSecs::now(),
        });
        assert!(
            super::due_checkins(&mut tasks, CHECKIN, at).0.is_empty(),
            "the worker says it checked in 10 s ago, though we saw nothing for 3·X"
        );
        let much_later = later(at, CHECKIN * 2);
        assert_eq!(
            super::due_checkins(&mut tasks, CHECKIN, much_later).0.len(),
            1,
            "the age in the beat grows with the time since it arrived"
        );
    }

    /// The clock runs only while the worker holds the ball, and a follow-up restarts it.
    #[test]
    fn the_clock_pauses_with_the_ball_on_the_sender_and_restarts_on_a_follow_up() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::InputRequired), false),
            start,
        );
        let waiting = later(start, CHECKIN * 10);
        assert!(
            super::due_checkins(&mut tasks, CHECKIN, waiting)
                .0
                .is_empty(),
            "a question waiting on the sender is no silence of the worker"
        );
        apply(&mut tasks, &leg(LegKind::Text, true), waiting);
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Working,
            "the answer resumes it"
        );
        assert!(
            super::due_checkins(&mut tasks, CHECKIN, later(waiting, JUST_BEFORE_TWO_X))
                .0
                .is_empty(),
            "the follow-up restarted the clock"
        );
        assert_eq!(
            super::due_checkins(&mut tasks, CHECKIN, later(waiting, CHECKIN * 2))
                .0
                .len(),
            1,
            "2·X after the follow-up the worker is overdue"
        );
    }

    /// A check-in resets both timers, so the alarm needs 2·X of silence again.
    #[test]
    fn a_check_in_resets_the_timers() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        assert_eq!(
            super::due_checkins(&mut tasks, CHECKIN, later(start, CHECKIN * 2))
                .0
                .len(),
            1,
            "the alarm fired"
        );
        let checked_in = later(start, CHECKIN * 2 + Duration::from_secs(5));
        super::ingest(
            &mut tasks,
            IngestLegParams {
                frame: &authored_by("calm-otter", &checkin_leg("built it", "50", false)),
                task_id: &tid(),
                mine: false,
                now: checked_in,
            },
        );
        assert!(
            super::due_checkins(&mut tasks, CHECKIN, later(checked_in, CHECKIN))
                .0
                .is_empty(),
            "a fresh check-in clears the alarm"
        );
        assert_eq!(
            super::due_checkins(&mut tasks, CHECKIN, later(checked_in, CHECKIN * 2))
                .0
                .len(),
            1,
            "2·X of silence after it raises the next alarm"
        );
    }

    /// A relay can deliver a leg again. The replay is told by its id, surfaced or not,
    /// and only the first copy moves the clock.
    #[test]
    fn a_replayed_leg_does_not_move_the_clock_on_the_push_path() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        let frame = checkin_leg("built it", "50", false);
        let first = later(start, Duration::from_secs(10));
        let replay = later(start, Duration::from_secs(100));
        for now in [first, replay] {
            super::ingest(
                &mut tasks,
                IngestLegParams {
                    frame: &authored_by("calm-otter", &frame),
                    task_id: &tid(),
                    mine: false,
                    now,
                },
            );
        }
        assert_eq!(
            tasks[&tid()].last_skill_activity,
            first,
            "the replay left the clock where the first copy set it"
        );
    }

    /// The RPC path of a follow-up reads the same record of seen legs.
    #[test]
    fn a_replayed_follow_up_does_not_move_the_clock_on_the_rpc_path() {
        let start = Instant::now();
        let mut tasks = task_we_serve(start);
        let message_id = "5c1c3a3e-4b0e-4b1c-9f0e-0a1b2c3d4e5f";
        let mut moved = Vec::new();
        for after in [10, 100] {
            let now = later(start, Duration::from_secs(after));
            let first_seen = super::mark_seen(&mut tasks, &tid(), message_id);
            let mut info = leg(LegKind::Text, false);
            info.first_seen = first_seen;
            apply(&mut tasks, &info, now);
            moved.push(tasks[&tid()].last_skill_activity);
        }
        assert_eq!(
            moved[1], moved[0],
            "the second copy of the follow-up is a replay"
        );
    }

    /// The set of seen legs is bounded: ten times its cap leaves the newest ids in and the
    /// oldest out.
    #[test]
    fn the_seen_legs_are_bounded() {
        let start = Instant::now();
        let mut tasks = task_we_serve(start);
        let cap = TASK_LEGS_SEEN_CAP;
        for index in 0..cap * 10 {
            super::mark_seen(&mut tasks, &tid(), &format!("leg-{index}"));
        }
        assert!(
            !super::mark_seen(&mut tasks, &tid(), &format!("leg-{}", cap * 10 - 1)),
            "the newest id is remembered"
        );
        assert!(
            super::mark_seen(&mut tasks, &tid(), "leg-0"),
            "the oldest id was forgotten, so the set did not grow"
        );
    }

    /// A beat is not a check-in: it never moves the clock.
    #[test]
    fn a_beat_is_not_a_check_in() {
        let start = Instant::now();
        let mut tasks = task_we_serve(start);
        apply(
            &mut tasks,
            &leg(LegKind::Beat, false),
            later(start, Duration::from_secs(40)),
        );
        assert_eq!(
            tasks[&tid()].last_skill_activity,
            start,
            "the beat left the clock alone"
        );
    }

    /// The reminder names the follow-up the worker has not answered, until any leg of its own
    /// answers it.
    #[test]
    fn the_reminder_carries_the_follow_up_seq_until_answered() {
        let start = Instant::now();
        let mut tasks = task_we_serve(start);
        tasks
            .get_mut(&tid())
            .expect("the test inserted this task earlier")
            .followup_seq = Some(41);
        let reminders = super::due_checkins(&mut tasks, CHECKIN, later(start, CHECKIN)).1;
        assert_eq!(
            reminders.first().map(|due| due.followup_seq),
            Some(Some(41)),
            "the reminder names the follow-up"
        );
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Working), true),
            later(start, CHECKIN * 2),
        );
        assert_eq!(
            tasks[&tid()].followup_seq,
            None,
            "a leg of the worker answers it"
        );
    }

    /// The check-in's text, progress and tag are kept per task, capped, and a check-in
    /// without a tag clears it.
    #[test]
    fn a_check_in_keeps_its_text_progress_and_tag() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        let first = UnixSecs::now();
        let mut waiting = checkin_leg("waits for review", "30", true);
        waiting.timestamp = first;
        ingest_inbound(&mut tasks, &waiting);
        let rec = &tasks[&tid()];
        assert_eq!(
            rec.status_text.as_deref(),
            Some("waits for review"),
            "the text of the check-in is kept"
        );
        assert_eq!(
            rec.last_progress,
            Some(progress_of(30)),
            "the progress of the check-in is kept"
        );
        assert!(rec.waiting_user, "the tag names whom the worker waits on");
        let mut plain = checkin_leg("review done", "40", false);
        plain.timestamp = first + Duration::from_secs(5);
        ingest_inbound(&mut tasks, &plain);
        assert!(
            !tasks[&tid()].waiting_user,
            "a check-in without the tag clears it"
        );
        let long = "x".repeat(TASK_STATUS_TEXT_MAX_CHARS * 2);
        let mut capped = checkin_leg(&long, "50", false);
        capped.timestamp = first + Duration::from_secs(10);
        ingest_inbound(&mut tasks, &capped);
        assert_eq!(
            tasks[&tid()].status_text.as_deref().map(str::len),
            Some(TASK_STATUS_TEXT_MAX_CHARS),
            "the text is capped"
        );
    }

    /// The sender wakes when a check-in moves the progress to another quarter, in
    /// either direction, and not otherwise.
    #[test]
    fn a_check_in_wakes_only_when_the_quarter_moves() {
        let mut tasks = working_task_we_opened(Instant::now());
        let moved =
            |registry: &mut HashMap<TaskId, TaskRecord>, held: Option<u8>, percent: &str| {
                registry
                    .get_mut(&tid())
                    .expect("the test inserted this task earlier")
                    .last_progress = held.map(progress_of);
                super::quarter_moved(
                    registry.get(&tid()),
                    &authored_by("calm-otter", &checkin_leg("step", percent, false)),
                )
            };
        assert!(!moved(&mut tasks, Some(30), "40"), "same quarter");
        assert!(
            !moved(&mut tasks, Some(25), "49"),
            "same quarter at both ends"
        );
        assert!(moved(&mut tasks, Some(24), "25"), "24 to 25 moves up");
        assert!(moved(&mut tasks, Some(60), "45"), "60 to 45 moves down");
        assert!(moved(&mut tasks, Some(99), "100"), "99 to 100 moves up");
        assert!(
            !moved(&mut tasks, None, "0"),
            "the first progress after none"
        );
        assert!(
            !moved(&mut tasks, None, "80"),
            "the first progress after none"
        );
    }

    /// A beat only replays the last progress, so it never moves the quarter.
    #[test]
    fn a_beat_with_a_new_progress_does_not_move_the_quarter() {
        let mut tasks = working_task_we_opened(Instant::now());
        tasks
            .get_mut(&tid())
            .expect("the test inserted this task earlier")
            .last_progress = Some(progress_of(10));
        let beat = status_frame(
            TaskState::Working,
            Some(status_update_params::beat_metadata(Some(progress_of(90)))),
        );
        assert!(
            !super::quarter_moved(tasks.get(&tid()), &authored_by("calm-otter", &beat)),
            "a beat never wakes"
        );
    }

    /// An artifact frame for `tid()` from the worker, stamped at `at`.
    fn artifact_leg(at: UnixSecs) -> Message {
        use habilis_network::protocol::{AppFrameParams, MeshId, MessageBody};

        let mesh = MeshId::from("test");
        let payload = status_update_params::artifact_update(&mesh, &tid(), "the result");
        let mut frame = Message::new_app(
            &mesh,
            &Nickname::from("calm-otter"),
            AppFrameParams::new(
                wire::app_tag(wire::ARTIFACT),
                Some(peer_key("worker-bot")),
                None,
                MessageBody::try_new(
                    &serde_json::to_string(&payload).expect("a test value serializes to JSON"),
                )
                .expect("a test value serializes to JSON"),
            ),
        );
        frame.timestamp = at;
        frame
    }

    /// A delivered result answers what the worker waited on, so a rework that then stalls
    /// raises an alarm that rings, not one filed under `waiting on user`.
    #[test]
    fn an_artifact_clears_what_the_worker_waited_on() {
        let first = UnixSecs::now();
        let mut tasks = working_task_we_opened(Instant::now());
        ingest_inbound(&mut tasks, &waiting_leg(true, first));
        assert!(tasks[&tid()].waiting_user, "the check-in names the user");
        ingest_inbound(&mut tasks, &artifact_leg(first + Duration::from_secs(5)));
        assert!(
            !tasks[&tid()].waiting_user,
            "the artifact ends the wait on the user"
        );
    }

    /// The follow-up hands the task back to the worker at dispatch, so the check-in clock runs
    /// again even when the request or its reply is lost.
    #[test]
    fn a_follow_up_resumes_a_task_that_waited_on_input() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        let parked = tasks
            .get_mut(&tid())
            .expect("the test inserted this task earlier");
        parked.state = TaskState::InputRequired;
        parked.review = true;
        super::note_follow_up_sent(
            &mut tasks,
            &tid(),
            &peer_key("calm-otter"),
            later(start, CHECKIN),
        );
        let resumed = &tasks[&tid()];
        assert_eq!(
            resumed.state,
            TaskState::Working,
            "the answer resumes the task"
        );
        assert!(!resumed.review, "nothing waits for a review any more");
        assert_eq!(
            resumed.ball(),
            TaskRole::Receiver,
            "the worker holds the ball"
        );
        assert_eq!(
            super::due_checkins(&mut tasks, CHECKIN, later(start, CHECKIN * 3))
                .0
                .len(),
            1,
            "a worker silent after the answer is overdue 2·X later"
        );
    }

    /// Ingest `frame` as the worker's at `now`, for tests that read the clocks.
    fn ingest_at(tasks: &mut HashMap<TaskId, TaskRecord>, frame: &Message, now: Instant) {
        super::ingest(
            tasks,
            IngestLegParams {
                frame: &authored_by("calm-otter", frame),
                task_id: &tid(),
                mine: false,
                now,
            },
        );
    }

    fn beat_leg(checkin_age_secs: u64, at: UnixSecs) -> Message {
        let extras = status_update_params::BeatExtras {
            checkin_age_secs: Some(checkin_age_secs),
            ..Default::default()
        };
        let mut frame = status_frame(
            TaskState::Working,
            Some(status_update_params::beat_metadata_with_extras(extras)),
        );
        frame.timestamp = at;
        frame
    }

    /// A frame from a member who is not the task's worker is not even remembered: it could push
    /// real leg ids out of the bounded set or forge the beat that silences the alarm.
    #[test]
    fn a_bystander_frame_changes_nothing_in_the_record() {
        let mut tasks = working_task_we_opened(Instant::now());
        let beat = beat_leg(0, UnixSecs::now());
        let working = checkin_leg("sneaky", "40", true);
        for frame in [&beat, &working] {
            super::ingest(
                &mut tasks,
                IngestLegParams {
                    frame: &authored_by("stranger-bot", frame),
                    task_id: &tid(),
                    mine: false,
                    now: Instant::now(),
                },
            );
        }
        let rec = &tasks[&tid()];
        assert!(
            rec.peer_beat.is_none(),
            "a stranger's beat is not the worker's"
        );
        assert!(
            !rec.legs_seen.contains(working.id.as_str()),
            "a stranger's leg id is not remembered"
        );
        assert!(
            !rec.waiting_user,
            "a stranger cannot name whom the worker waits on"
        );
    }

    /// A relay can deliver beat 1 after beat 2; the older one must not shrink the stated age.
    #[test]
    fn a_beat_delivered_late_does_not_replace_a_newer_one() {
        let sent = UnixSecs::now();
        let mut tasks = working_task_we_opened(Instant::now());
        let now = Instant::now();
        ingest_at(
            &mut tasks,
            &beat_leg(90, sent + Duration::from_secs(10)),
            now,
        );
        ingest_at(&mut tasks, &beat_leg(0, sent), now);
        assert_eq!(
            tasks[&tid()]
                .peer_beat
                .and_then(|beat| beat.checkin_age_secs),
            Some(90),
            "the beat sent first is not news"
        );
    }

    /// A check-in that a slow relay delivers after a newer one is a first sight of its id, but not
    /// news, so it cannot postpone the alarm.
    #[test]
    fn a_check_in_delivered_late_does_not_restart_the_clock() {
        let sent = UnixSecs::now();
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        let newer = later(start, Duration::from_secs(1));
        ingest_at(
            &mut tasks,
            &waiting_leg(false, sent + Duration::from_secs(10)),
            newer,
        );
        ingest_at(&mut tasks, &waiting_leg(false, sent), later(start, CHECKIN));
        assert_eq!(
            tasks[&tid()].last_skill_activity,
            newer,
            "the older check-in leaves the clock where the newer one put it"
        );
    }

    /// The age in a beat comes from the peer and has no bound; it must not panic the sender.
    #[test]
    fn an_unbounded_beat_age_cannot_panic_the_alarm() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        tasks
            .get_mut(&tid())
            .expect("the test inserted this task earlier")
            .peer_beat = Some(super::PeerBeat {
            checkin_age_secs: Some(u64::MAX),
            at: start,
            sent_at: UnixSecs::now(),
        });
        assert_eq!(
            super::due_checkins(&mut tasks, CHECKIN, later(start, CHECKIN * 2))
                .0
                .len(),
            1,
            "the local age wins over an absurd stated age"
        );
    }

    /// An adopted reply refreshes the reaper clocks but not the check-in clock, so a sender that
    /// probes more often than 2·X still gets its alarm; only a follow-up restarts it.
    #[test]
    fn an_adopted_reply_does_not_delay_the_alarm() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        let probe = later(start, CHECKIN);
        super::adopt_initiator(
            &mut tasks,
            super::AdoptInitiatorParams {
                task_id: &tid(),
                peer_key: &peer_key("calm-otter"),
                peer: &Nickname::from("calm-otter"),
                task_state: TaskState::Working,
                now: probe,
            },
        );
        assert_eq!(
            tasks[&tid()].last_peer_activity,
            probe,
            "the probe still refreshes the reaper clock"
        );
        assert_eq!(
            super::due_checkins(&mut tasks, CHECKIN, later(start, CHECKIN * 2))
                .0
                .len(),
            1,
            "the alarm is due 2·X after the last check-in, whatever the probes"
        );
    }

    /// The alarm says whether the worker never accepted the task.
    #[test]
    fn an_unaccepted_task_is_overdue_as_not_picked_up() {
        let start = Instant::now();
        let mut tasks = HashMap::new();
        open_as_initiator(&mut tasks, start);
        let overdue = super::due_checkins(&mut tasks, CHECKIN, later(start, CHECKIN * 2)).0;
        assert_eq!(
            overdue.first().map(|notice| notice.not_picked_up),
            Some(true),
            "a submitted task is not picked up"
        );
    }
    /// A working status leg for `tid()` at wire time `at`, with a waiting reason
    /// when one is given.
    fn waiting_leg(waiting_user: bool, at: UnixSecs) -> Message {
        let mut frame = status_frame(
            TaskState::Working,
            status_update_params::waiting_metadata(waiting_user),
        );
        frame.timestamp = at;
        frame
    }

    fn ingest_inbound(registry: &mut HashMap<TaskId, TaskRecord>, frame: &Message) {
        super::ingest(
            registry,
            IngestLegParams {
                frame: &authored_by("calm-otter", frame),
                task_id: &tid(),
                mine: false,
                now: Instant::now(),
            },
        );
    }

    /// A relay can deliver an old status leg hours late. Both the `--waiting-on`
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
        };
        let waiting = waiting_leg(true, first);
        let clear_same_second = waiting_leg(false, first);
        let clear = waiting_leg(false, first + Duration::from_secs(10));
        let waiting_again = waiting_leg(true, first + Duration::from_secs(20));

        deliver(&mut tasks, &waiting);
        deliver(&mut tasks, &clear_same_second);
        assert!(
            !tasks[&tid()].waiting_user,
            "no waiting question before the leg"
        );
        ingest_inbound(&mut tasks, &waiting);
        assert!(
            !tasks[&tid()].waiting_user,
            "a replayed --waiting-on leg of the same second must not set the reason again"
        );

        deliver(&mut tasks, &clear);
        ingest_inbound(&mut tasks, &waiting);
        assert!(
            !tasks[&tid()].waiting_user,
            "a replayed --waiting-on leg must not set the reason again"
        );

        deliver(&mut tasks, &waiting_again);
        assert!(
            tasks[&tid()].waiting_user,
            "the waiting question is the newest one"
        );
        ingest_inbound(&mut tasks, &clear);
        assert!(
            tasks[&tid()].waiting_user,
            "a replayed older plain leg must not clear the reason"
        );
    }

    /// `ingest` guards the same-second replay itself: a caller that does not
    /// reaches `mark_seen` too.
    #[test]
    fn ingest_alone_ignores_a_same_second_waiting_replay() {
        let at = UnixSecs::now();
        let mut tasks = working_task_we_opened(Instant::now());
        let waiting = waiting_leg(true, at);
        let clear = waiting_leg(false, at);

        ingest_inbound(&mut tasks, &waiting);
        assert!(
            tasks[&tid()].waiting_user,
            "the waiting question is the one asked"
        );
        ingest_inbound(&mut tasks, &clear);
        assert!(
            !tasks[&tid()].waiting_user,
            "the clear leg removes the waiting question"
        );

        ingest_inbound(&mut tasks, &waiting);
        assert!(
            !tasks[&tid()].waiting_user,
            "the replayed --waiting-on leg must not set the reason again"
        );
    }

    /// A working status leg for `tid()` at wire time `at`, with the worker's
    /// order value and a waiting reason when one is given.
    fn ordered_leg(waiting_user: bool, at: UnixSecs, order_ms: u64) -> Message {
        let mut metadata = serde_json::json!({ "mesh:order_ms": order_ms });
        if waiting_user {
            metadata["mesh:waiting_user"] = true.into();
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
        let waiting = ordered_leg(true, at, 1_000);
        let clear = ordered_leg(false, at, 2_000);

        ingest_inbound(&mut tasks, &clear);
        ingest_inbound(&mut tasks, &waiting);
        assert!(
            !tasks[&tid()].waiting_user,
            "the older waiting leg must not restore its reason after the clear"
        );
    }

    /// A leg without an order value, from a peer that does not send one, is
    /// ordered by its wire second, as before.
    #[test]
    fn a_leg_without_an_order_value_is_ordered_by_its_second() {
        let at = UnixSecs::now();
        let mut tasks = working_task_we_opened(Instant::now());
        ingest_inbound(&mut tasks, &ordered_leg(false, at, 2_000));

        ingest_inbound(&mut tasks, &waiting_leg(true, at + Duration::from_secs(1)));
        assert!(
            tasks[&tid()].waiting_user,
            "the waiting question is the one asked"
        );
        ingest_inbound(&mut tasks, &waiting_leg(false, at));
        assert!(
            tasks[&tid()].waiting_user,
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
            &ordered_leg(true, at + Duration::from_secs(1), 1_000),
        );
        assert!(
            tasks[&tid()].waiting_user,
            "the waiting question is kept after an out of order leg"
        );

        ingest_inbound(&mut tasks, &ordered_leg(false, at, 2_000));
        assert!(
            !tasks[&tid()].waiting_user,
            "the clear with the higher order value must win"
        );
    }

    /// A reason set by a leg without an order value is ordered by its second
    /// against a later leg that does carry one.
    #[test]
    fn an_ordered_leg_falls_back_to_the_second_after_an_unordered_waiting_leg() {
        let at = UnixSecs::now();
        let mut tasks = working_task_we_opened(Instant::now());
        ingest_inbound(&mut tasks, &waiting_leg(true, at + Duration::from_secs(1)));

        ingest_inbound(&mut tasks, &ordered_leg(false, at, 5_000));
        assert!(
            tasks[&tid()].waiting_user,
            "an older second with an order value must not clear a reason set without one"
        );
        ingest_inbound(
            &mut tasks,
            &ordered_leg(false, at + Duration::from_secs(2), 6_000),
        );
        assert!(
            !tasks[&tid()].waiting_user,
            "the waiting question is cleared"
        );
    }

    /// The contract: only a `working` leg carries the reason. A leg for any
    /// other live state clears it, even when it carries `mesh:waiting_user`.
    #[test]
    fn only_a_working_leg_sets_the_waiting_reason() {
        let first = UnixSecs::now();
        let mut tasks = working_task_we_opened(Instant::now());
        let metadata = || status_update_params::waiting_metadata(true);
        let mut leg = status_frame(TaskState::InputRequired, metadata());
        leg.timestamp = first;
        ingest_inbound(&mut tasks, &leg);
        assert_eq!(
            tasks[&tid()].state,
            TaskState::InputRequired,
            "the task waits for input"
        );
        assert!(
            !tasks[&tid()].waiting_user,
            "an input-required leg must not set the reason"
        );
    }

    /// A finished task is frozen: a late `--waiting-on` leg sets nothing.
    #[test]
    fn a_late_waiting_leg_on_a_finished_task_sets_nothing() {
        let mut tasks = working_task_we_opened(Instant::now());
        ingest_inbound(&mut tasks, &status_frame(TaskState::Completed, None));
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Completed,
            "a leg after the terminal state is ignored"
        );

        ingest_inbound(&mut tasks, &waiting_leg(true, UnixSecs::now()));
        assert!(
            !tasks[&tid()].waiting_user,
            "the waiting question is cleared after completion"
        );
    }

    /// A beat older than one idle timeout proves nothing about the worker now: a
    /// relay can re-inject it after the worker died, so it cannot enable the alarm.
    #[test]
    fn a_stale_beat_enables_no_alarm() {
        let now = Instant::now();
        let mut tasks = working_task_we_opened(now);
        let mut beat = status_frame(
            TaskState::Working,
            Some(status_update_params::beat_metadata_with_extras(
                status_update_params::BeatExtras {
                    checkin_age_secs: Some(1),
                    ..Default::default()
                },
            )),
        );
        beat.timestamp = UnixSecs::now() - Duration::from_secs(super::task_timeout_secs() + 1);
        ingest_inbound(&mut tasks, &beat);
        assert!(
            tasks[&tid()].peer_beat.is_none(),
            "a replayed beat proves nothing"
        );
    }

    /// A working leg with `mesh:waiting_user` records the flag, and the next
    /// status leg clears it.
    #[test]
    fn a_status_leg_sets_and_clears_the_waiting_reason() {
        let now = Instant::now();
        let mut tasks = working_task_we_opened(now);
        let ingest_leg = |registry: &mut HashMap<TaskId, TaskRecord>, frame| {
            super::ingest(
                registry,
                IngestLegParams {
                    frame: &authored_by("calm-otter", &frame),
                    task_id: &tid(),
                    mine: false,
                    now,
                },
            );
        };

        let waiting = serde_json::json!({ extensions::META_WAITING_USER: true });
        ingest_leg(&mut tasks, status_frame(TaskState::Working, Some(waiting)));
        assert!(
            tasks[&tid()].waiting_user,
            "the waiting question is the one asked"
        );

        ingest_leg(&mut tasks, status_frame(TaskState::Working, None));
        assert!(
            !tasks[&tid()].waiting_user,
            "the waiting question is cleared"
        );
    }

    /// A leg on a task that already closed is a replay: a relay delivered an
    /// old frame after the engine forgot its id. Pre-fix it surfaced again,
    /// so agents saw `completed` twice and stale `working` text after it.
    #[test]
    fn a_leg_on_a_closed_task_is_a_replay() {
        let now = Instant::now();
        let mut tasks = HashMap::new();
        open_as_initiator(&mut tasks, now);
        tasks
            .get_mut(&tid())
            .expect("the test inserted this task earlier")
            .state = TaskState::Completed;
        let late = status_leg(false);
        assert!(
            super::is_replay(&tasks, &closed(), &tid(), &late.id),
            "a relay can deliver an old frame, and a closed task must not surface it twice"
        );
    }

    /// The record goes 2 min after close, and the replays in the field came
    /// 45 min to 4 h late, so the sweep remembers what it reaped.
    #[test]
    fn a_leg_on_a_reaped_task_is_a_replay() {
        let timeout = Duration::from_mins(2);
        let now = Instant::now();
        let mut tasks = HashMap::new();
        open_as_initiator(&mut tasks, now);
        let rec = tasks
            .get_mut(&tid())
            .expect("the test inserted this task earlier");
        rec.state = TaskState::Completed;
        rec.last_activity = now
            .checked_sub(timeout * 2)
            .expect("the test instant is far from the clock origin");

        let mut reaped_ids = closed();
        let (_, reaped) = super::sweep_registry(&mut tasks, &mut reaped_ids, now, timeout);
        assert_eq!(reaped, vec![tid()], "the reaped task list holds the task");
        assert!(tasks.is_empty(), "the registry is empty after the reap");
        let late = status_leg(false);
        assert!(
            super::is_replay(&tasks, &reaped_ids, &tid(), &late.id),
            "the reap forgets the record but the id list must still catch old frames"
        );
    }

    /// After the sweep reaps a finished task, its own peer can still read it
    /// over gossip. Pre-fix the record was gone and only the id was kept, so a
    /// peer that missed the terminal event got "task not found" and could not
    /// tell a finished task from a lost one. A bystander still gets the refusal.
    #[test]
    fn a_reaped_task_is_still_served_to_its_own_peer() {
        use habilis_network::protocol::MeshId;
        use serde_json::json;

        use crate::a2a::a2a_app::A2aApp;
        use crate::a2a::rpc_error::A2aOp;
        use crate::a2a::served::{Served, classify};

        let timeout = Duration::from_mins(2);
        let now = Instant::now();
        let mut app = A2aApp::new();
        open_as_initiator(&mut app.tasks, now);
        let rec = app
            .tasks
            .get_mut(&tid())
            .expect("the test inserted this task earlier");
        rec.state = TaskState::Completed;
        rec.last_activity = now
            .checked_sub(timeout * 2)
            .expect("the test instant is far from the clock origin");
        super::sweep_registry(&mut app.tasks, &mut app.closed_tasks, now, timeout);
        assert!(app.tasks.is_empty(), "the app has no tasks after the reap");

        let params = json!({"id": tid().as_str()});
        assert!(
            matches!(
                classify("GetTask", &params, &peer_key("calm-otter"), &app),
                Served::Op(A2aOp::GetTask { .. })
            ),
            "the task's own peer must still be served"
        );
        assert!(
            matches!(
                classify("GetTask", &params, &peer_key("wire-thistle"), &app),
                Served::Reject(_)
            ),
            "another peer must still get task not found"
        );

        let task = rpc_error::get_task(&app, &tid(), &MeshId::from("test"))
            .expect("the test inserted this task earlier");
        assert_eq!(
            task["id"],
            tid().as_str(),
            "the task id is the requested id"
        );
        assert_eq!(
            task["status"]["state"], "TASK_STATE_COMPLETED",
            "the task state is completed"
        );
        assert_eq!(
            task["metadata"]["mesh:peer"], "calm-otter",
            "the task keeps the peer metadata"
        );
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
                    peer_key: peer_key("calm-otter"),
                    peer: Nickname::from("calm-otter"),
                    state: TaskState::Failed,
                },
            );
        }
        assert!(
            closed.get(&ids[0]).is_none(),
            "the oldest closed task is evicted"
        );
        assert!(
            closed.get(&ids[1]).is_some(),
            "the second closed task is kept"
        );
        assert!(
            closed.get(&ids[2]).is_some(),
            "the newest closed task is kept"
        );
    }

    #[test]
    fn the_same_leg_surfaces_once() {
        let now = Instant::now();
        let mut tasks = HashMap::new();
        open_as_initiator(&mut tasks, now);
        let working = status_leg(false);
        super::mark_seen(&mut tasks, &tid(), working.id.as_str());
        assert!(
            super::is_replay(&tasks, &closed(), &tid(), &working.id),
            "a leg for a closed task is a replay"
        );
    }

    /// A peer beats every 30 s with a fresh id. Recorded, the beats would push
    /// every real id out of the bounded set in about 32 min, well inside the
    /// replay window.
    #[test]
    fn beats_do_not_evict_legs_seen() {
        let now = Instant::now();
        let mut tasks = HashMap::new();
        open_as_initiator(&mut tasks, now);
        let real = status_leg(false);
        super::mark_seen(&mut tasks, &tid(), real.id.as_str());
        for _ in 0..100 {
            ingest_inbound(&mut tasks, &status_leg(true));
        }
        assert!(
            super::is_replay(&tasks, &closed(), &tid(), &real.id),
            "a leg the task already saw is a replay"
        );
    }

    /// Pin: a new leg on a live task surfaces, and so does a leg for a task we
    /// do not know yet (the worker's first leg can beat the RPC response).
    #[test]
    fn a_new_leg_and_an_unknown_task_surface() {
        let now = Instant::now();
        let mut tasks = HashMap::new();
        open_as_initiator(&mut tasks, now);
        super::mark_seen(&mut tasks, &tid(), status_leg(false).id.as_str());
        let fresh = status_leg(false);
        assert!(
            !super::is_replay(&tasks, &closed(), &tid(), &fresh.id),
            "a live task must keep surfacing its new legs"
        );
        let unknown = TaskId::from("660e8400-e29b-41d4-a716-446655440000");
        assert!(
            !super::is_replay(&tasks, &closed(), &unknown, &fresh.id),
            "the worker's first leg can beat the RPC response, so it must still surface"
        );
    }

    /// The record a sender holds once its brief is answered: the task, as an initiator, at
    /// `Submitted`.
    fn open_as_initiator(tasks: &mut HashMap<TaskId, TaskRecord>, now: Instant) {
        tasks.entry(tid()).or_insert_with(|| {
            TaskRecord::open(
                (peer_key("calm-otter"), Nickname::from("calm-otter")),
                TaskRole::Initiator,
                TaskState::Submitted,
                now,
            )
        });
    }

    fn leg(kind: LegKind, mine: bool) -> LegInfo<'static> {
        // Leak a fixed peer/task_id for 'static convenience in tests.
        LegInfo {
            task_id: Box::leak(Box::new(tid())),
            peer_key: Box::leak(Box::new(peer_key("calm-otter"))),
            peer: Some(Box::leak(Box::new(Nickname::from("calm-otter")))),
            kind,
            mine,
            progress: None,
            first_seen: true,
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
        let outsider_key = Box::leak(Box::new(peer_key("wire-thistle")));
        let from_outsider = |kind: LegKind| LegInfo {
            task_id: Box::leak(Box::new(tid())),
            peer_key: outsider_key,
            peer: Some(outsider),
            kind,
            mine: false,
            progress: None,
            first_seen: true,
        };

        // We are the *initiator* — the side the attack lands on, since an
        // inbound leg is credited to the worker — of a task `calm-otter`
        // accepted.
        let mut tasks = HashMap::new();
        open_as_initiator(&mut tasks, now);
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Working), false),
            now,
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Working,
            "the task is working before the peer clock check"
        );
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
        assert!(
            !tasks[&tid()].review,
            "the review flag is clear before the review"
        );

        let later = now
            .checked_add(Duration::from_mins(30))
            .expect("the test instant cannot overflow");
        apply(&mut tasks, &from_outsider(LegKind::Beat), later);
        assert_eq!(
            tasks[&tid()].last_peer_activity,
            peer_clock,
            "a non-party's beat cannot refresh the reaper's clock"
        );

        // The real counterparty still drives it.
        apply(&mut tasks, &leg(LegKind::Artifact, false), now);
        assert_eq!(
            tasks[&tid()].state,
            TaskState::InputRequired,
            "the task waits for review"
        );
        assert!(tasks[&tid()].review, "the review flag is set");
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Completed), false),
            now,
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Completed,
            "the approval completes the task"
        );
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
        let flooder = peer_key("wire-thistle");
        let honest = peer_key("calm-otter");
        let mut tasks = HashMap::new();
        let now = Instant::now();

        let mut open = |peer_key: &AuthorKey, index: usize| {
            let task_id = TaskId::from(format!("550e8400-e29b-41d4-a716-{index:012}").as_str());
            apply(
                &mut tasks,
                &LegInfo {
                    task_id: &task_id,
                    peer_key,
                    peer: Some(&Nickname::from("a-peer")),
                    kind: LegKind::Offer,
                    mine: false,
                    progress: None,
                    first_seen: true,
                },
                now,
            );
            task_id
        };

        let mut minted = Vec::new();
        for index in 0..tuning::TASKS_PER_PEER_CAP {
            minted.push(open(&flooder, index));
        }

        assert_eq!(
            admit_new_task(&tasks, &flooder),
            Admission::PeerAtCap,
            "the flooder is refused at the peer cap"
        );
        assert_eq!(
            admit_new_task(&tasks, &honest),
            Admission::Ok,
            "one peer's flood must not lock another peer out"
        );

        // The quota counts live tasks, so finished ones free it again — an
        // orchestrator running many short tasks with one worker is not a
        // flooder, and must not be treated as one.
        tasks
            .get_mut(&minted[0])
            .expect("the test inserted this task earlier")
            .state = TaskState::Completed;
        assert_eq!(
            admit_new_task(&tasks, &flooder),
            Admission::Ok,
            "a new task is admitted under the cap"
        );
    }

    /// An initiator-side adoption respects the registry cap, as a remote offer does.
    ///
    /// Pre-fix `adopt_initiator` inserted with no admission check, so the registry
    /// grew past `TASKS_CAP` and every remote offer was refused after.
    #[test]
    fn an_initiator_adoption_past_the_cap_is_refused() {
        let mut tasks = HashMap::new();
        let now = Instant::now();
        for index in 0..10 * tuning::TASKS_CAP {
            let task_id = TaskId::from(format!("550e8400-e29b-41d4-a716-{index:012}").as_str());
            let peer = Nickname::from(format!("peer-{}", index % 64).as_str());
            super::adopt_initiator(
                &mut tasks,
                super::AdoptInitiatorParams {
                    task_id: &task_id,
                    peer_key: &AuthorKey::from([u8::try_from(index % 64).expect("below 64"); 32]),
                    peer: &peer,
                    task_state: TaskState::Working,
                    now,
                },
            );
        }
        assert_eq!(
            tasks.len(),
            tuning::TASKS_CAP,
            "10 times the cap was offered: the registry holds the cap, no more and no less"
        );
        let last = TaskId::from(
            format!("550e8400-e29b-41d4-a716-{:012}", 10 * tuning::TASKS_CAP - 1).as_str(),
        );
        assert!(
            !tasks.contains_key(&last),
            "the last offered task was refused, not kept"
        );
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
        // A peer idle well past the debounce.
        let rec = tasks
            .get_mut(&tid())
            .expect("the test inserted this task earlier");
        rec.last_peer_activity = now.checked_sub(timeout * 2).expect("test clock");

        let (expired, reaped) = super::sweep_registry(&mut tasks, &mut closed(), now, timeout);

        assert_eq!(expired.len(), 1, "the idle task is cancelled");
        assert!(
            reaped.is_empty(),
            "the cancel needs one more pass to be seen before the record disappears"
        );
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
        assert_eq!(now_reaped, vec![tid()], "the reaped id is the stale task");
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
        open_as_initiator(&mut tasks, long_ago);
        // Our daemon beat a moment ago; the worker, which beats, has said
        // nothing since.
        let rec = tasks
            .get_mut(&tid())
            .expect("the test inserted this task earlier");
        rec.last_activity = now;

        let (expired, _) = super::sweep_registry(&mut tasks, &mut closed(), now, timeout);
        assert_eq!(
            expired.len(),
            1,
            "a silent peer times out despite our beats"
        );

        // A beat from the peer keeps it live.
        let mut beaten = HashMap::new();
        open_as_initiator(&mut beaten, long_ago);
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
        open_as_initiator(&mut tasks, now);
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
                    peer_key: &peer_key(peer),
                    peer: &Nickname::from(peer),
                    task_state: state,
                    now: when,
                },
            );
        };

        let later = now
            .checked_add(Duration::from_mins(5))
            .expect("the test instant cannot overflow");
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
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Failed,
            "the real worker holds the task, so its snapshot is the one that may close it"
        );
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
                peer_key: Box::leak(Box::new(peer_key("wire-thistle"))),
                peer: Some(Box::leak(Box::new(Nickname::from("wire-thistle")))),
                kind: LegKind::Offer,
                mine: false,
                progress: None,
                first_seen: true,
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
        let departed = peer_key("calm-otter");
        let offer = |task_id: &'static str, peer: &'static str| LegInfo {
            task_id: Box::leak(Box::new(TaskId::from(task_id))),
            peer_key: Box::leak(Box::new(peer_key(peer))),
            peer: Some(Box::leak(Box::new(Nickname::from(peer)))),
            kind: LegKind::Offer,
            mine: false,
            progress: None,
            first_seen: true,
        };
        let live_id = "550e8400-e29b-41d4-a716-446655440001";
        let staying_id = "550e8400-e29b-41d4-a716-446655440002";
        let done_id = "550e8400-e29b-41d4-a716-446655440003";

        let mut tasks = HashMap::new();
        apply(&mut tasks, &offer(live_id, "calm-otter"), now);
        apply(&mut tasks, &offer(staying_id, "drift-oak"), now);
        apply(&mut tasks, &offer(done_id, "calm-otter"), now);
        tasks
            .get_mut(&TaskId::from(done_id))
            .expect("the test inserted this task earlier")
            .state = TaskState::Completed;

        let out = crate::emission::Output::silent();
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
        use habilis_network::protocol::{AppFrameParams, MeshId, Message, MessageBody};

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
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Working,
            "the task keeps working after the echoed artifact"
        );

        // The worker's own echoed artifact leg: directed, body is a sealed
        // ciphertext stand-in that does not parse as a `TaskArtifactUpdate`.
        let sealed = Message::new_app(
            &mesh,
            &Nickname::from("worker-bot"),
            AppFrameParams::new(
                wire::app_tag(wire::ARTIFACT),
                Some(peer_key("calm-otter")),
                None,
                MessageBody::try_new("sealed-ciphertext-stand-in").expect("valid body"),
            ),
        );
        assert!(
            status_update_params::frame_task_id(&sealed).is_none(),
            "a sealed body yields no parseable task id — the condition under test"
        );

        // ②: the self-echo still advances the record (id threaded, not parsed).
        super::ingest(
            &mut tasks,
            IngestLegParams {
                frame: &authored_by("worker-bot", &sealed),
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
        assert!(tasks[&tid()].review, "the task is parked for review");

        // ①: the app surfaces the *plaintext* logical frame it built (the same
        // `task_id`, unsealed), so rendering that self-echo as JSON must not
        // panic and must carry the id.
        let payload = status_update_params::artifact_update(&mesh, &task_id, "result text");
        let plaintext = Message::new_app(
            &mesh,
            &Nickname::from("worker-bot"),
            AppFrameParams::new(
                wire::app_tag(wire::ARTIFACT),
                Some(peer_key("calm-otter")),
                None,
                MessageBody::try_new(&serde_json::to_string(&payload).expect("payload serializes"))
                    .expect("valid body"),
            ),
        );
        let line = crate::emission::event_json(&crate::emission::OutputEvent::Task {
            message: Box::new(plaintext.sign(&Identity::generate()).authored()),
            is_self: true,
            to_name: None,
            quarter_moved: false,
        })
        .expect("a task event renders a JSON line");
        let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid JSON");
        assert_eq!(
            parsed["task_id"],
            task_id.as_str(),
            "the frame names the task"
        );
        assert_eq!(parsed["self"], true, "the frame is marked self");
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
        use habilis_network::protocol::{AppFrameParams, MeshId, Message, MessageBody};

        let mesh = MeshId::from("test");
        let task_id = tid();
        let now = Instant::now();

        // The worker's own status leg, as the plaintext twin (`sealed: false`)
        // or as the wire frame the addressee alone can read (`sealed: true`).
        let status_frame = |state: TaskState, sealed: bool| {
            let body = if sealed {
                "sealed-ciphertext-stand-in".to_owned()
            } else {
                let payload = status_update_params::status_update(
                    &mesh,
                    status_update_params::StatusUpdateParams::new(&task_id, state),
                );
                serde_json::to_string(&payload).expect("payload serializes")
            };
            Message::new_app(
                &mesh,
                &Nickname::from("worker-bot"),
                AppFrameParams::new(
                    wire::app_tag(wire::STATUS),
                    Some(peer_key("calm-otter")),
                    None,
                    MessageBody::try_new(&body).expect("valid body"),
                ),
            )
        };
        let ingest_own = |tasks: &mut HashMap<_, _>, frame: &Message| {
            super::ingest(
                tasks,
                IngestLegParams {
                    frame: &authored_by("worker-bot", frame),
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
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Working,
            "the task keeps working after the unreadable frame"
        );

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
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Completed,
            "the plaintext twin carries the state that the sealed body hides"
        );
        assert!(
            tasks[&tid()].state.is_terminal(),
            "the completed task is terminal"
        );

        // And so the idle sweep passes it over, however long it sits there.
        // This is `tick_task_sweep`'s expiry predicate, verbatim.
        let timeout = Duration::from_secs(task_record::task_timeout_secs());
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

        open_as_initiator(&mut tasks, now);
        let rec = &tasks[&tid()];
        assert_eq!(rec.role, TaskRole::Initiator, "the initiator role is set");
        assert_eq!(rec.state, TaskState::Submitted, "the new task is submitted");
        assert_eq!(
            rec.ball(),
            TaskRole::Receiver,
            "a fresh offer waits on the worker to accept it"
        );

        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Working), false),
            now,
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Working,
            "the worker's accept moves the task out of submitted"
        );
        assert_eq!(
            tasks[&tid()].ball(),
            TaskRole::Receiver,
            "the worker is busy, so the initiator has nothing to do yet"
        );

        // Worker asks a question → input-required, ball flips to me.
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::InputRequired), false),
            now,
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::InputRequired,
            "the task waits for input"
        );
        assert_eq!(
            tasks[&tid()].ball(),
            TaskRole::Initiator,
            "the worker asked a question, so only the initiator can move the task on"
        );
        assert!(!tasks[&tid()].review, "the review flag is clear");

        // My answer resumes the work and hands the ball back.
        apply(&mut tasks, &leg(LegKind::Text, true), now);
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Working,
            "the task resumes working"
        );
        assert_eq!(
            tasks[&tid()].ball(),
            TaskRole::Receiver,
            "the initiator answered, so the worker acts again"
        );

        // The result parks the task for my review.
        apply(&mut tasks, &leg(LegKind::Artifact, false), now);
        assert_eq!(
            tasks[&tid()].state,
            TaskState::InputRequired,
            "the task waits for input"
        );
        assert!(tasks[&tid()].review, "the review flag is set");
        assert_eq!(
            tasks[&tid()].ball(),
            TaskRole::Initiator,
            "a returned result waits for the initiator to review it"
        );

        // My approval message resumes the worker...
        apply(&mut tasks, &leg(LegKind::Text, true), now);
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Working,
            "the task resumes working"
        );
        assert_eq!(
            tasks[&tid()].ball(),
            TaskRole::Receiver,
            "the confirm sends the worker back to work"
        );

        // ...and the *worker* authors the terminal `completed` (native A2A).
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Completed), false),
            now,
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Completed,
            "the worker's final status closes the task"
        );
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
        open_as_initiator(&mut tasks, now);
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Submitted,
            "a completed leg before any accept must not close the task"
        );

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
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Completed,
            "after its own accept, the worker may close the task"
        );

        // Declining an offer outright is a different leg, and still lands.
        let mut declined = HashMap::new();
        open_as_initiator(&mut declined, now);
        apply(
            &mut declined,
            &leg(LegKind::Status(TaskState::Failed), false),
            now,
        );
        assert_eq!(
            declined[&tid()].state,
            TaskState::Failed,
            "a decline is a different leg from a close, so it must still land"
        );
    }

    /// A terminal task is frozen: a later leg never mutates it.
    #[test]
    fn terminal_is_frozen() {
        let mut tasks = HashMap::new();
        let now = Instant::now();
        open_as_initiator(&mut tasks, now);
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Rejected), false),
            now,
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Rejected,
            "a stray confirm from the initiator must not resurrect a closed task"
        );
        // A stray confirm after decline must not resurrect it.
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Completed), true),
            now,
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Rejected,
            "the task stays rejected"
        );
    }

    /// A non-offer leg for an unknown task id is dropped (no record created).
    #[test]
    fn unknown_task_non_offer_is_ignored() {
        let mut tasks = HashMap::new();
        apply(&mut tasks, &leg(LegKind::Text, false), Instant::now());
        assert!(tasks.is_empty(), "the registry is empty");
    }

    /// Out-of-order legs (a confirm before any review park) are ignored.
    #[test]
    fn out_of_order_is_ignored() {
        let mut tasks = HashMap::new();
        let now = Instant::now();
        open_as_initiator(&mut tasks, now);
        // A confirm straight after the offer — no review to close.
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Completed), true),
            now,
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Submitted,
            "a confirm with no review park has nothing to close"
        );
    }

    /// Direction guards (native A2A — the worker drives its own task):
    /// an *initiator*-authored `working` (accepting is the worker's) and an
    /// *initiator*-authored `completed` (the worker closes the task) are both
    /// ignored; the worker's own `completed` is what closes it.
    #[test]
    fn misdirected_statuses_are_ignored() {
        let mut tasks = HashMap::new();
        let now = Instant::now();
        open_as_initiator(&mut tasks, now);
        // Initiator cannot accept its own offer.
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Working), true),
            now,
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Submitted,
            "only the worker can accept an offer, so the initiator's working leg is dropped"
        );
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
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Completed,
            "only the worker's own completed leg closes the task"
        );
    }

    /// A revision loop: artifact → review park → the initiator's `change`
    /// message resumes the work → a second artifact parks again.
    #[test]
    fn change_loops_back_to_working() {
        let mut tasks = HashMap::new();
        let now = Instant::now();
        open_as_initiator(&mut tasks, now);
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Working), false),
            now,
        );
        apply(&mut tasks, &leg(LegKind::Artifact, false), now);
        assert!(tasks[&tid()].review, "the review flag is set");
        apply(&mut tasks, &leg(LegKind::Text, true), now); // change verdict
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Working,
            "the task is working after the verdict change"
        );
        assert!(!tasks[&tid()].review, "the review flag is clear");
        apply(&mut tasks, &leg(LegKind::Artifact, false), now);
        assert_eq!(
            tasks[&tid()].state,
            TaskState::InputRequired,
            "the task waits for input"
        );
        assert!(tasks[&tid()].review, "the review flag is set");
    }

    /// A nickname is a label, so a second key can use the name of the counterparty. The frame
    /// names `calm-otter` as its author, but another key signed it: it is a non-party leg.
    #[test]
    fn a_leg_from_another_key_with_the_name_of_the_counterparty_is_dropped() {
        let now = Instant::now();
        let mut tasks = working_task_we_opened(now);
        let peer_clock = tasks[&tid()].last_peer_activity;
        let completed = status_frame(TaskState::Completed, None);

        let from_twin = authored_by("calm-otter-twin", &completed);
        super::ingest(
            &mut tasks,
            IngestLegParams {
                frame: &from_twin,
                task_id: &tid(),
                mine: false,
                now: now + Duration::from_secs(5),
            },
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Working,
            "a key that only shares the name cannot close the task"
        );
        assert_eq!(
            tasks[&tid()].last_peer_activity,
            peer_clock,
            "nor refresh the clock that the reaper reads"
        );

        let from_peer = authored_by("calm-otter", &completed);
        super::ingest(
            &mut tasks,
            IngestLegParams {
                frame: &from_peer,
                task_id: &tid(),
                mine: false,
                now,
            },
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Completed,
            "the key of the counterparty still closes the task"
        );
    }

    /// The same rule for the snapshot that an RPC reply returns.
    #[test]
    fn a_returned_task_is_not_adopted_from_a_key_that_shares_the_worker_name() {
        let now = Instant::now();
        let mut tasks = working_task_we_opened(now);
        super::adopt_initiator(
            &mut tasks,
            super::AdoptInitiatorParams {
                task_id: &tid(),
                peer_key: &peer_key("calm-otter-twin"),
                peer: &Nickname::from("calm-otter"),
                task_state: TaskState::Failed,
                now: now + Duration::from_mins(5),
            },
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Working,
            "a twin of the worker cannot drive our task terminal"
        );
    }

    /// The quota counts keys, not names: a key that shares the name of a flooder is another peer.
    #[test]
    fn the_task_quota_counts_keys_and_not_names() {
        use super::{Admission, admit_new_task};
        let flooder = peer_key("wire-thistle");
        let twin = peer_key("wire-thistle-twin");
        let mut tasks = HashMap::new();
        for index in 0..tuning::TASKS_PER_PEER_CAP {
            let task_id = TaskId::from(format!("550e8400-e29b-41d4-a716-{index:012}").as_str());
            apply(
                &mut tasks,
                &LegInfo {
                    task_id: &task_id,
                    peer_key: &flooder,
                    peer: Some(&Nickname::from("wire-thistle")),
                    kind: LegKind::Offer,
                    mine: false,
                    progress: None,
                    first_seen: true,
                },
                Instant::now(),
            );
        }
        assert_eq!(
            admit_new_task(&tasks, &flooder),
            Admission::PeerAtCap,
            "the live tasks of one key fill its quota"
        );
        assert_eq!(
            admit_new_task(&tasks, &twin),
            Admission::Ok,
            "a key with the same name has its own quota"
        );
    }

    /// Two keys can use one nickname. When one of them leaves, only its tasks end.
    #[test]
    fn peer_left_cancels_only_the_tasks_of_the_key_that_left() {
        let now = Instant::now();
        let offer = |task_id: &'static str, key_name: &'static str| LegInfo {
            task_id: Box::leak(Box::new(TaskId::from(task_id))),
            peer_key: Box::leak(Box::new(peer_key(key_name))),
            peer: Some(Box::leak(Box::new(Nickname::from("calm-otter")))),
            kind: LegKind::Offer,
            mine: false,
            progress: None,
            first_seen: true,
        };
        let left_id = "550e8400-e29b-41d4-a716-446655440001";
        let twin_id = "550e8400-e29b-41d4-a716-446655440002";
        let mut tasks = HashMap::new();
        apply(&mut tasks, &offer(left_id, "calm-otter"), now);
        apply(&mut tasks, &offer(twin_id, "calm-otter-twin"), now);

        let out = crate::emission::Output::silent();
        super::fail_tasks_for_departed_peer(&mut tasks, &peer_key("calm-otter"), &out);

        assert_eq!(
            tasks[&TaskId::from(left_id)].state,
            TaskState::Canceled,
            "the departed key loses its live task at once"
        );
        assert!(
            !tasks[&TaskId::from(twin_id)].state.is_terminal(),
            "the task of the key that stays is untouched, though it shares the name"
        );
    }
}
