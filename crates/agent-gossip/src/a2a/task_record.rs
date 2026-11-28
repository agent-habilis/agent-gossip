//! The daemon-side **task** state machine + its two timers, over the A2A
//! lifecycle.
//!
//! A task is a directed, multi-leg conversation whose wire form is pure A2A:
//! a `message/send` with `mesh:seq` 1 opens it — the initiator mints the id and
//! the worker returns the `Task` — then `TaskStatusUpdate`s and mid-task `message/send`s
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

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use habilis_network::embed::{EventLoopState, HandlerCtx};
use habilis_network::ops::blob::ContentId;
use habilis_network::protocol::messages::Authored;
use habilis_network::protocol::{AuthorKey, Message, MessageKind, Nickname, UnixSecs};

use super::extensions::META_SEQ;
use super::status_update_params::{BeatExtras, Progress};
use super::{META_REASON, TaskId, TaskState, status_update_params, wire};
use crate::a2a::a2a_app::A2aApp;
use crate::a2a::tuning::{
    task_checkin_secs, task_keepalive_secs, task_skill_silence_max_secs, task_timeout_secs,
};
use crate::a2a::{broadcast_params, tuning};
use crate::emission;
use crate::output_event::{CheckinNotice, GapNotice, OverdueNotice};

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
    /// The progress of the last check-in the record took in. The quarter wake compares against
    /// it, so a beat never moves it.
    pub last_progress: Option<Progress>,
    /// The progress to show and to replay on a beat. A check-in sets it, and a beat of the
    /// worker repairs it on the sender's record, where a lost check-in would leave it behind.
    pub shown_progress: Option<Progress>,
    /// The seq of the observation that set `shown_progress`, a check-in or a beat. A later
    /// observation takes it over only with a higher seq, so a beat that a relay delivers late
    /// cannot show an older progress.
    pub shown_seq: u64,
    /// Whether the worker waits on its own user, from the `mesh:waiting_user` key of its last
    /// status leg. A label only: it does not move the check-in clock. A check-in without it
    /// clears it.
    pub waiting_user: bool,
    /// What changed, from the last check-in leg, capped. Not replayed.
    pub status_text: Option<String>,
    /// Whether the cap cut `status_text`.
    pub status_truncated: bool,
    /// The initiator's one-line name for the task, as the brief carried it.
    pub label: Option<String>,
    /// The brief or follow-up the worker has not answered yet, so a reminder can repeat it.
    /// Cleared by any leg the worker sends.
    pub unanswered: Option<Unanswered>,
    /// When this node last raised a reminder (worker) or an alarm (sender), so
    /// one task raises one per interval. Any check-in activity clears it.
    pub last_notice: Option<Instant>,
    /// The last beat of the worker, for the alarm line.
    pub peer_beat: Option<PeerBeat>,
    /// The `mesh:seq` of the last leg that this node sent in the task. The initiator's opening is
    /// 1 and each follow-up one more; the worker numbers its status and artifact legs from 1. The
    /// peer's counter is separate: it is [`peer_seq`](Self::peer_seq).
    pub seq: u64,
    /// The highest `mesh:seq` among the own legs that the record has taken. A leg takes its seq
    /// when it is built and its state when it has been delivered, so the two differ while a leg
    /// is in flight. A beat, `GetTask` and an end state this one: the state they carry is the
    /// state of the record, so the seq must be too.
    pub applied_seq: u64,
    /// The highest `mesh:seq` that this node took in from the peer. A leg of the peer is news only
    /// above it: a replay and a leg that arrives behind a newer one are dropped whole.
    pub peer_seq: u64,
    /// When this node asked the worker about a leg that a beat showed and that never arrived.
    /// One ask is in flight per task: the reply clears it, and one whose reply never came
    /// (refused, timed out, or lost) lapses after one call timeout, so a later beat asks again.
    pub asked_at: Option<Instant>,
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
            last_progress: None,
            shown_progress: None,
            shown_seq: 0,
            waiting_user: false,
            status_text: None,
            status_truncated: false,
            label: None,
            unanswered: None,
            last_notice: None,
            peer_beat: None,
            seq: 0,
            applied_seq: 0,
            peer_seq: 0,
            asked_at: None,
        }
    }

    /// End the task in `state`. Stamping `last_activity` starts the freeze window the GC waits
    /// for, so a late leg is still recognized as a duplicate and not as an unknown task.
    pub(crate) fn close(&mut self, state: TaskState, now: Instant) {
        self.state = state;
        self.review = false;
        self.waiting_user = false;
        self.last_activity = now;
    }

    /// End the task as canceled by this node without a leg of its own to carry it (the peer
    /// left). The end takes the next seq, as the cancel of the sweep does, so that an end sent
    /// again later is no repeat of a seq that the peer may already hold.
    pub(crate) fn cancel_without_leg(&mut self, now: Instant) {
        self.close(TaskState::Canceled, now);
        self.take_seq_applied(self.seq.saturating_add(1));
    }

    /// What the beat of this record adds. Only the worker states an age, a progress and a seq: the
    /// sender's silence is not a check-in, a progress the sender holds is a copy of the worker's,
    /// and the sender's beat has no seq. The seq is that of the state held, so a leg in flight
    /// shows no seq yet.
    pub(crate) fn beat_extras(&self, now: Instant) -> BeatExtras {
        let from_worker = self.role == TaskRole::Receiver;
        BeatExtras {
            progress: if from_worker {
                self.shown_progress
            } else {
                None
            },
            checkin_age_secs: from_worker
                .then(|| now.duration_since(self.last_skill_activity).as_secs()),
            seq: from_worker.then_some(self.applied_seq),
        }
    }

    /// Count `seq` as sent and applied at once: a leg that the record takes as it is built.
    pub(crate) fn take_seq_applied(&mut self, seq: u64) {
        self.seq = self.seq.max(seq);
        self.applied_seq = self.applied_seq.max(seq);
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
    /// The task-creating `message/send` (`mesh:seq` 1) — the initiator minted the id and opened
    /// its record at dispatch; the worker opens its own when the message arrives.
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
    /// Whether the leg is news, as [`Applied::news`] says. A leg that is not news changes
    /// neither the state nor a clock.
    pub news: bool,
}

/// The value cluster [`ingest`] needs beyond its `tasks` registry handle.
#[derive(Clone, Copy)]
pub(crate) struct IngestLegParams<'caller> {
    pub(crate) frame: &'caller Authored,
    pub(crate) task_id: &'caller TaskId,
    pub(crate) mine: bool,
    pub(crate) now: Instant,
}

/// Feed one **validated logical** task frame into the registry, from the broadcast
/// (`mine = true`) or the receive (`mine = false`) path, with no reaped task listed. A frame that
/// is not a directed app frame belongs to no task and is a no-op.
///
/// On the `mine` path the caller must hand us the plaintext twin of the leg, not the wire frame:
/// the wire body is sealed to the addressee, and a status leg reads its state out of the body.
pub(crate) fn ingest(
    tasks: &mut HashMap<TaskId, TaskRecord>,
    params: IngestLegParams<'_>,
) -> Applied {
    let IngestLegParams {
        frame,
        task_id,
        mine,
        now,
    } = params;
    let MessageKind::App { to: Some(to), .. } = &frame.kind else {
        return Applied::NONE;
    };
    let author = frame.key();
    let leg = Leg {
        task_id,
        peer_key: if mine { to } else { &author },
        peer: None,
        source: LegSource::Frame(frame),
        mine,
        now,
    };
    apply_leg(tasks, &ClosedTasks::new(0), leg)
}

/// A leg of one task, as the state machine takes it in.
pub(crate) struct Leg<'caller> {
    pub task_id: &'caller TaskId,
    /// The other party's key: the author of a leg that arrived, the addressee of one we sent.
    pub peer_key: &'caller AuthorKey,
    /// The other party's name, which a brief records.
    pub peer: Option<&'caller Nickname>,
    pub source: LegSource<'caller>,
    /// `true` if we sent the leg (the echo of our own), `false` if it arrived.
    pub mine: bool,
    pub now: Instant,
}

/// Where a leg comes from.
pub(crate) enum LegSource<'caller> {
    /// A status or artifact frame.
    Frame(&'caller Authored),
    /// A message of the RPC path, by its `mesh:seq`: a brief when it opens the task, else a
    /// follow-up.
    Message { seq: u64, opens_task: bool },
}

/// What a beat of the peer asks of this node, for the layer that sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BeatAction {
    /// The beat of the worker shows a leg that never arrived, and this node has not asked the
    /// worker about that seq yet: ask for the task.
    Ask,
    /// The beat reached a task that this node ended by its own leg, which the peer never got: send
    /// the end again.
    Resend,
}

/// What a received leg turned out to be, for the layer that surfaces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Applied {
    /// Whether the leg is news: its `mesh:seq` is above the highest that the record took in from
    /// the peer, and its task is still open. A leg that is not news is dropped whole: it stays off
    /// the surface, and it moves no state and no clock.
    pub news: bool,
    /// Whether a check-in put the progress in another quarter.
    pub quarter_moved: bool,
    /// What a beat of the peer calls for, if it calls for anything.
    pub beat: Option<BeatAction>,
}

impl Applied {
    /// A leg that belongs to no task, or to a task of another peer.
    const NONE: Self = Self {
        news: false,
        quarter_moved: false,
        beat: None,
    };
}

/// What the state machine reads out of a leg.
struct Facts {
    kind: LegKind,
    /// The `mesh:seq` that the sender stamped. A leg without one cannot be placed in the
    /// sequence. A beat repeats the seq of the last leg of its worker, and takes no place in the
    /// sequence.
    seq: Option<u64>,
    /// The note of a status leg, which is what the worker said last: a check-in's text, or the
    /// question it asked.
    text: Option<status_update_params::CappedText>,
    checkin: Option<CheckinFields>,
    beat: Option<(PeerBeat, Option<Progress>)>,
}

/// Read a frame. `None` for a frame that is no task leg, one whose payload does not parse, and
/// a beat that proves nothing now.
fn facts_of_frame(frame: &Authored, mine: bool, now: Instant) -> Option<Facts> {
    let MessageKind::App { tag, .. } = &frame.kind else {
        return None;
    };
    let mut facts = Facts {
        kind: LegKind::Artifact,
        seq: None,
        text: None,
        checkin: None,
        beat: None,
    };
    match tag.as_str() {
        wire::STATUS => {
            let payload = status_update_params::status_payload(frame).ok()?;
            if status_update_params::is_beat(&payload) {
                // A beat older than one idle timeout proves nothing about the peer now.
                if !mine && is_stale(frame) {
                    return None;
                }
                facts.kind = LegKind::Beat;
                facts.seq = status_update_params::status_seq(&payload);
                facts.beat = Some((
                    PeerBeat {
                        checkin_age_secs: status_update_params::beat_checkin_age(&payload),
                        at: now,
                        sent_at: frame.timestamp,
                    },
                    status_update_params::leg_progress(&payload),
                ));
            } else {
                if payload.status.state == TaskState::Working {
                    facts.checkin = Some(CheckinFields {
                        waiting_user: status_update_params::status_waiting_user(&payload),
                        progress: status_update_params::leg_progress(&payload),
                    });
                }
                facts.text = status_update_params::status_text(&payload);
                facts.seq = status_update_params::status_seq(&payload);
                facts.kind = LegKind::Status(payload.status.state);
            }
        }
        wire::ARTIFACT => {
            facts.seq = status_update_params::artifact_payload(frame)
                .ok()
                .and_then(|payload| status_update_params::artifact_seq(&payload));
        }
        _ => return None,
    }
    Some(facts)
}

fn facts_of_message(seq: u64, opens_task: bool) -> Facts {
    Facts {
        kind: if opens_task {
            LegKind::Offer
        } else {
            LegKind::Text
        },
        seq: Some(seq),
        text: None,
        checkin: None,
        beat: None,
    }
}

/// Apply one leg to the registry: the one place that decides whether a leg is news, for a frame
/// and for a message of the RPC path alike, whoever sent it. A leg of the peer is news only when
/// its `mesh:seq` is above the highest that the record took in: a replay, and a leg that arrives
/// behind a newer one, are dropped whole, and so is a leg with no seq. A beat takes no part in the
/// sequence. A task that closed (or that the sweep reaped) has no news, and neither has a task
/// that we hold no record of: the initiator opened its record at dispatch. A leg that is not news
/// moves no state and no clock.
pub(crate) fn apply_leg(
    tasks: &mut HashMap<TaskId, TaskRecord>,
    closed: &ClosedTasks,
    leg: Leg<'_>,
) -> Applied {
    let Leg {
        task_id,
        peer_key,
        peer,
        source,
        mine,
        now,
    } = leg;
    let facts = match source {
        LegSource::Frame(frame) => match facts_of_frame(frame, mine, now) {
            Some(facts) => facts,
            None => return Applied::NONE,
        },
        LegSource::Message { seq, opens_task } => facts_of_message(seq, opens_task),
    };
    // A reaped task is over: a leg of it, an opening included, must not mint a record again.
    // A beat of the peer on it still shows that the peer never got its end.
    if !tasks.contains_key(task_id)
        && let Some(reaped) = closed.get(task_id)
    {
        let resend =
            facts.kind == LegKind::Beat && !mine && reaped.owes_end && reaped.peer_key == *peer_key;
        return Applied {
            beat: resend.then_some(BeatAction::Resend),
            ..Applied::NONE
        };
    }
    let taken_in = tasks.get(task_id).map_or(0, |rec| rec.peer_seq);
    let fresh = mine || facts.kind == LegKind::Beat || facts.seq.is_some_and(|seq| seq > taken_in);
    let reach = apply(
        tasks,
        &LegInfo {
            task_id,
            peer_key,
            peer,
            kind: facts.kind,
            mine,
            news: fresh,
        },
        now,
    );
    // A leg of another peer changes nothing, and neither does any leg of a task that was over
    // before it: only the leg that closes a task still moves its record. A leg that the state
    // machine does not accept is no news and takes no seq in.
    if reach == Reach::Finished && facts.kind == LegKind::Beat && !mine {
        let resend = tasks
            .get(task_id)
            .is_some_and(|rec| owes_end(rec) && rec.peer_key == *peer_key);
        return Applied {
            beat: resend.then_some(BeatAction::Resend),
            ..Applied::NONE
        };
    }
    if matches!(reach, Reach::Foreign | Reach::Finished | Reach::Ignored) {
        return Applied::NONE;
    }
    if reach == Reach::Live
        && fresh
        && !mine
        && facts.kind != LegKind::Beat
        && let (Some(seq), Some(record)) = (facts.seq, tasks.get_mut(task_id))
    {
        record.peer_seq = seq;
    }
    if reach == Reach::Live
        && mine
        && let (Some(seq), Some(record)) = (facts.seq, tasks.get_mut(task_id))
    {
        record.applied_seq = record.applied_seq.max(seq);
    }
    let from_peer_beat = facts.kind == LegKind::Beat && !mine;
    let quarter_moved = record_frame_facts(tasks, task_id, &facts, (mine, fresh));
    let finished = closed.get(task_id).is_some();
    let repair = from_peer_beat
        && tasks
            .get_mut(task_id)
            .is_some_and(|rec| asks_for_repair(rec, facts.seq, now));
    Applied {
        news: fresh && !finished && reach != Reach::Unknown,
        quarter_moved: quarter_moved && !finished,
        beat: repair.then_some(BeatAction::Ask),
    }
}

/// Whether a beat that repeats the seq `beat_seq` shows a leg that this node never took in, on a
/// task that is open, with no ask about it in flight. It marks the ask, so a task has one ask in
/// flight and a worker that beats fast costs one question at a time.
fn asks_for_repair(rec: &mut TaskRecord, beat_seq: Option<u64>, now: Instant) -> bool {
    let call_timeout = Duration::from_secs(tuning::CALL_TIMEOUT_SECS);
    let in_flight = rec
        .asked_at
        .is_some_and(|at| now.duration_since(at) < call_timeout);
    let shows_missing = beat_seq.is_some_and(|seq| seq > rec.peer_seq);
    if in_flight || !shows_missing || rec.role != TaskRole::Initiator || rec.state.is_terminal() {
        return false;
    }
    rec.asked_at = Some(now);
    true
}

/// Forget that this node asked about `task_id`, because the ask never left: the next beat that
/// shows the missing leg asks again.
pub(crate) fn forget_ask(tasks: &mut HashMap<TaskId, TaskRecord>, task_id: &TaskId) {
    if let Some(rec) = tasks.get_mut(task_id) {
        rec.asked_at = None;
    }
}

/// Whether this node ended the task by a leg of its own, which a peer that still beats never got:
/// a worker ends it with a terminal status, and either side ends it with a cancel. A task that
/// the peer ended is no business of this node to announce.
fn owes_end(rec: &TaskRecord) -> bool {
    rec.state.is_terminal() && (rec.state == TaskState::Canceled || rec.role == TaskRole::Receiver)
}

/// Keep what a frame says beyond the state: the age of a beat and the progress it replays, and
/// the flag, the text and the progress of a check-in. Says whether a check-in moved the quarter.
fn record_frame_facts(
    tasks: &mut HashMap<TaskId, TaskRecord>,
    task_id: &TaskId,
    facts: &Facts,
    (mine, fresh): (bool, bool),
) -> bool {
    let Some(rec) = tasks.get_mut(task_id) else {
        return false;
    };
    // Only a beat from the other party says anything about its age, and a beat that a relay
    // delivers behind a newer one, or again, says nothing. The progress that the beat replays
    // follows the seq instead of the clock: the sender's record takes it when the seq is above
    // the one that set what it shows, which repairs a check-in that never arrived. The worker's
    // own record is the authority on its progress.
    if !mine && let Some((beat, progress)) = facts.beat {
        if rec.peer_beat.is_none_or(|held| held.sent_at < beat.sent_at) {
            rec.peer_beat = Some(beat);
        }
        if rec.role == TaskRole::Initiator
            && progress.is_some()
            && let Some(seq) = facts.seq.filter(|seq| *seq > rec.shown_seq)
        {
            rec.shown_progress = progress;
            rec.shown_seq = seq;
        }
    }
    // Every working leg restates whom the worker waits on, so a plain one clears it. A relay can
    // replay an old leg, so only a leg above the highest seq taken in updates it. A finished task
    // waits for nothing.
    let from_worker = mine == (rec.role == TaskRole::Receiver);
    let current = from_worker && fresh;
    let mut quarter_moved = false;
    match facts.kind {
        LegKind::Status(_) if rec.state.is_terminal() => rec.waiting_user = false,
        LegKind::Status(_) if current => {
            quarter_moved = facts
                .checkin
                .as_ref()
                .is_some_and(|checkin| quarter_changed(rec.last_progress, checkin.progress));
            if let Some(capped) = &facts.text {
                rec.status_text = Some(capped.text.clone());
                rec.status_truncated = capped.truncated;
            }
            record_checkin(rec, facts.checkin.as_ref(), facts.seq);
        }
        // A delivered result answers whatever the worker waited on, and no check-in needs to
        // say so.
        LegKind::Artifact if current && !rec.state.is_terminal() => {
            rec.waiting_user = false;
        }
        LegKind::Offer | LegKind::Text | LegKind::Beat | LegKind::Status(_) | LegKind::Artifact => {
        }
    }
    quarter_moved
}

/// Keep what a status leg says about the work. A check-in sets the flag, the
/// text and the progress it carries; any other status leg clears the flag.
fn record_checkin(rec: &mut TaskRecord, checkin: Option<&CheckinFields>, seq: Option<u64>) {
    let Some(checkin) = checkin else {
        rec.waiting_user = false;
        return;
    };
    rec.waiting_user = checkin.waiting_user;
    if checkin.progress.is_some() {
        rec.last_progress = checkin.progress;
        // A beat that overtook this check-in may have shown a newer progress already.
        if let Some(seq) = seq.filter(|seq| *seq > rec.shown_seq) {
            rec.shown_progress = checkin.progress;
            rec.shown_seq = seq;
        }
    }
}

/// The check-in fields of a `working` status leg.
struct CheckinFields {
    waiting_user: bool,
    progress: Option<Progress>,
}

/// Whether a check-in puts the progress in another quarter than the one held. The first
/// progress after none does not count.
fn quarter_changed(held: Option<Progress>, new: Option<Progress>) -> bool {
    matches!(
        (held.map(Progress::quarter), new.map(Progress::quarter)),
        (Some(held), Some(new)) if held != new
    )
}

/// A beat older than one idle timeout proves nothing about the peer now. The
/// engine dedups by a bounded count, not by age, so a relay could re-inject an
/// old beat after the peer died. The bound is coarse on purpose, to tolerate
/// clock skew between hosts.
fn is_stale(frame: &Message) -> bool {
    match clock_skew(frame.timestamp) {
        Some(ClockSkew::Behind) => true,
        Some(ClockSkew::Ahead) => {
            warn_clock_ahead(&frame.author, frame.timestamp);
            true
        }
        None => false,
    }
}

/// The least time between two warnings that a clock runs ahead, so that a peer whose clock is off
/// does not write a line for every beat it sends.
const CLOCK_AHEAD_WARN_SECS: i64 = 300;

/// The second of the last warning that a clock runs ahead. One value for the whole process bounds
/// the warnings, whoever sends the beats.
static LAST_CLOCK_AHEAD_WARNING: AtomicI64 = AtomicI64::new(i64::MIN);

/// Warn that `author`'s clock, which stamped a beat at `sent_at`, runs ahead of ours: the beat was
/// ignored. At most one warning in [`CLOCK_AHEAD_WARN_SECS`], with the skew it saw.
fn warn_clock_ahead(author: &Nickname, sent_at: UnixSecs) {
    let now = UnixSecs::now();
    let last = LAST_CLOCK_AHEAD_WARNING.load(Ordering::Relaxed);
    if now.as_secs().saturating_sub(last) < CLOCK_AHEAD_WARN_SECS {
        return;
    }
    let claimed = LAST_CLOCK_AHEAD_WARNING.compare_exchange(
        last,
        now.as_secs(),
        Ordering::Relaxed,
        Ordering::Relaxed,
    );
    if claimed.is_ok() {
        let ahead = sent_at.since(now);
        tracing::warn!(
            target: "agent_gossip::a2a",
            %author,
            ahead_secs = ahead.as_secs(),
            "a beat from a clock that is ahead of ours by more than the tolerance was ignored: \
             the clocks of the two hosts differ"
        );
    }
}

/// How far a sender's clock is off ours, when it is off by more than the tolerance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClockSkew {
    /// What was sent is older than one idle timeout.
    Behind,
    /// What was sent is ahead of our clock by more than [`SKEW_FLOOR_SECS`] or one idle timeout,
    /// whichever is larger.
    Ahead,
}

/// The least that a clock may run ahead of ours before what it sends is ignored, whatever the idle
/// timeout: a host a minute off is healthy, and a low `--task-timeout-secs` must not turn it away.
const SKEW_FLOOR_SECS: u64 = 120;

/// Whether something sent at `sent_at` is older than one idle timeout, so that a relay could be
/// replaying it, or ahead of our clock, so that it would pin the order of what comes after it.
pub(crate) fn clock_skew(sent_at: UnixSecs) -> Option<ClockSkew> {
    skew_between(sent_at, UnixSecs::now(), task_timeout_secs())
}

/// [`clock_skew`] for a given `now` and idle timeout `timeout_secs`.
fn skew_between(sent_at: UnixSecs, now: UnixSecs, timeout_secs: u64) -> Option<ClockSkew> {
    if now.since(sent_at).as_secs() > timeout_secs {
        Some(ClockSkew::Behind)
    } else if sent_at.since(now).as_secs() > timeout_secs.max(SKEW_FLOOR_SECS) {
        Some(ClockSkew::Ahead)
    } else {
        None
    }
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

/// Whether `text` holds nothing but whitespace.
fn is_blank(text: &str) -> bool {
    text.trim().is_empty()
}

/// The brief or follow-up that the worker has not answered, as a reminder repeats it.
///
/// The text is peer-controlled and bound for the worker's agent, so it loses its control
/// characters except the newline and is capped at `TASK_UNANSWERED_TEXT_MAX_CHARS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unanswered {
    pub text: String,
    /// Whether the cap cut the text.
    pub truncated: bool,
    /// Whether the message carries a file, which the reminder does not repeat.
    pub has_file: bool,
    /// Whether the message is the opening brief, which the worker never picked up, and not a
    /// follow-up.
    pub brief: bool,
}

impl Unanswered {
    pub(crate) fn of(message: &super::Message, brief: bool) -> Self {
        let lines: Vec<&str> = message
            .parts
            .iter()
            .filter_map(|part| part.text.as_deref())
            .collect();
        let capped = status_update_params::capped_text(
            &lines.join("\n"),
            tuning::TASK_UNANSWERED_TEXT_MAX_CHARS,
        );
        let (text, truncated) =
            capped.map_or_else(Default::default, |capped| (capped.text, capped.truncated));
        let has_file = message
            .parts
            .iter()
            .any(|part| part.raw.is_some() || part.url.is_some());
        Self {
            text,
            truncated,
            has_file,
            brief,
        }
    }
}

/// The alarms and reminders that fall due at `now`, each marked so the next
/// one waits a full interval. The alarm is the sender's, from 2·X after the
/// last activity; the reminder is the worker's, from X. A worker whose beats
/// carry no check-in age counts the silence it sees itself.
pub(crate) fn due_checkins(
    tasks: &mut HashMap<TaskId, TaskRecord>,
    interval: Duration,
    now: Instant,
) -> (Vec<OverdueNotice>, Vec<CheckinNotice>) {
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
                if age >= interval.saturating_mul(2) {
                    rec.last_notice = Some(now);
                    let not_picked_up = rec.state == TaskState::Submitted;
                    let label = rec.label.clone();
                    let waiting_user = rec.waiting_user;
                    overdue.push(OverdueNotice {
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
                    let unanswered = rec.unanswered.as_ref();
                    let text: Option<String> = unanswered
                        .map(|unanswered| unanswered.text.clone())
                        .filter(|text| !is_blank(text));
                    let truncated = unanswered.is_some_and(|unanswered| unanswered.truncated);
                    let has_file = unanswered.is_some_and(|unanswered| unanswered.has_file);
                    let brief = unanswered.is_some_and(|unanswered| unanswered.brief);
                    reminders.push(CheckinNotice {
                        task_id: task_id.clone(),
                        peer: rec.peer.clone(),
                        text,
                        truncated,
                        has_file,
                        brief,
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
    for notice in &reminders {
        app.output.checkin_due(notice);
    }
}

/// What a reaped task keeps: enough to answer its own peer's `GetTask`, not
/// the full record.
pub(crate) struct ClosedTask {
    pub peer_key: AuthorKey,
    pub peer: Nickname,
    pub state: TaskState,
    /// The seq of the last leg that this node sent in the task, so that its snapshot still
    /// tells a peer how far the task got, and an end that it sends again keeps its place.
    pub seq: u64,
    /// Whether this node ended the task by a leg of its own, which a peer that still beats never
    /// got: see [`owes_end`].
    pub owes_end: bool,
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

/// What the worker's `Task` says about its task: the part of it that a record takes in.
pub(crate) struct Snapshot {
    pub(crate) state: TaskState,
    /// The seq of the last leg of the worker, `mesh:seq` of the task. A snapshot without one says
    /// nothing about how far the worker got, and is not taken in.
    pub(crate) seq: Option<u64>,
    pub(crate) progress: Option<Progress>,
    /// What the worker said last, which is the question when it asked one.
    pub(crate) text: Option<status_update_params::CappedText>,
    pub(crate) waiting_user: bool,
    pub(crate) review: bool,
}

/// The value cluster [`reconcile_initiator`] needs beyond its `tasks` registry handle.
pub(crate) struct ReconcileParams<'caller> {
    pub(crate) task_id: &'caller TaskId,
    pub(crate) peer_key: &'caller AuthorKey,
    pub(crate) snapshot: Snapshot,
    pub(crate) now: Instant,
}

/// Reconcile the **initiator**-side record of a task with the authoritative `Task` that its worker
/// returned over RPC (`SendMessage` or `GetTask`). The record exists from dispatch, so a reply
/// never creates one: a task that this node holds no record of is left alone. A terminal record is
/// frozen.
///
/// A snapshot whose seq is above the highest that the record took in says that legs were lost on
/// the way, and the snapshot stands for them: the record takes in its state, progress, text and
/// flags, and counts the seq as taken in, so the legs that come later are judged against it. One
/// path serves every lost leg. No check-in clock moves: the snapshot is not an agent's leg.
///
/// Returns the event for a sender that must answer when the snapshot moved the task into
/// `input-required`, since the question that put it there is the one leg that cannot be repaired
/// without the agent reading it again.
pub(crate) fn reconcile_initiator(
    tasks: &mut HashMap<TaskId, TaskRecord>,
    params: ReconcileParams<'_>,
) -> Option<GapNotice> {
    let ReconcileParams {
        task_id,
        peer_key,
        snapshot,
        now,
    } = params;
    let rec = tasks.get_mut(task_id)?;
    // Only from the task's own worker. The caller has already proved this response answers a call
    // we made to `peer_key`, but that says nothing about *which* task the answer may name: any
    // peer we call once could otherwise return a Task id belonging to a different peer and drive
    // it terminal, or keep refreshing it past the reaper. The record's own counterparty is the
    // authority.
    if rec.peer_key != *peer_key || rec.role != TaskRole::Initiator || rec.state.is_terminal() {
        return None;
    }
    // A reply shows that the worker is alive, and it moves the reaper's clocks and no other.
    rec.last_activity = now;
    rec.last_peer_activity = now;
    // Any reply ends the ask in flight, whether or not it carries news: the next beat that shows
    // a missing leg asks again.
    rec.asked_at = None;
    let seq = snapshot.seq.filter(|seq| *seq > rec.peer_seq)?;
    rec.peer_seq = seq;
    if snapshot.state.is_terminal() {
        rec.close(snapshot.state, now);
    } else {
        rec.state = snapshot.state;
        rec.review = snapshot.review;
        rec.waiting_user = snapshot.waiting_user;
    }
    if let Some(capped) = &snapshot.text {
        rec.status_text = Some(capped.text.clone());
        rec.status_truncated = capped.truncated;
    }
    if snapshot.progress.is_some() && seq > rec.shown_seq {
        rec.shown_progress = snapshot.progress;
        rec.shown_seq = seq;
    }
    let parked = snapshot.state == TaskState::InputRequired;
    (parked || snapshot.state.is_terminal()).then(|| gap_notice(task_id, rec, &snapshot))
}

/// The notice for a snapshot that moved the task where a lost leg would have: into a question, or
/// to its end. A snapshot in review carries no text: what the worker said last is then an older
/// check-in, and the result that parked the task is the leg that was lost.
fn gap_notice(task_id: &TaskId, rec: &TaskRecord, snapshot: &Snapshot) -> GapNotice {
    let capped = snapshot.text.as_ref().filter(|_| !rec.review);
    let text = capped.map(|capped| capped.text.clone());
    GapNotice {
        task_id: task_id.clone(),
        peer: rec.peer.clone(),
        label: rec.label.clone(),
        state: snapshot.state,
        text,
        truncated: capped.is_some_and(|capped| capped.truncated),
        review: rec.review,
    }
}

/// Record a follow-up we just sent into a task we opened, with the `mesh:seq` it carried. It
/// hands the task back to the worker at dispatch, not on the reply, so a lost reply or request
/// leaves no record parked on us with the check-in clock stopped. The seq is consumed here and
/// nowhere before, so a follow-up that was never sent leaves the counter where it was.
pub(crate) fn note_follow_up_sent(
    tasks: &mut HashMap<TaskId, TaskRecord>,
    task_id: &TaskId,
    peer_key: &AuthorKey,
    (seq, now): (u64, Instant),
) {
    let Some(rec) = tasks.get_mut(task_id).filter(|rec| {
        rec.role == TaskRole::Initiator && rec.peer_key == *peer_key && !rec.state.is_terminal()
    }) else {
        return;
    };
    rec.take_seq_applied(seq);
    apply(
        tasks,
        &LegInfo {
            task_id,
            peer_key,
            peer: None,
            kind: LegKind::Text,
            mine: true,
            news: true,
        },
        now,
    );
}

/// The seq that the next leg of `task_id` takes, when we hold the task. Nothing is consumed until
/// [`take_seq`], so a leg that fails before it leaves leaves the counter where it was.
pub(crate) fn next_seq(tasks: &HashMap<TaskId, TaskRecord>, task_id: &TaskId) -> Option<u64> {
    tasks.get(task_id)?.seq.checked_add(1)
}

/// Count `seq` as sent: the leg that carries it is about to leave.
pub(crate) fn take_seq(tasks: &mut HashMap<TaskId, TaskRecord>, task_id: &TaskId, seq: u64) {
    if let Some(rec) = tasks.get_mut(task_id) {
        rec.seq = rec.seq.max(seq);
    }
}

/// Give `seq` back when its leg failed before it left, unless a later leg took a higher one.
pub(crate) fn give_back_seq(tasks: &mut HashMap<TaskId, TaskRecord>, task_id: &TaskId, seq: u64) {
    if let Some(rec) = tasks.get_mut(task_id)
        && rec.seq == seq
    {
        rec.seq = seq.saturating_sub(1);
    }
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
pub(crate) fn apply(
    tasks: &mut HashMap<TaskId, TaskRecord>,
    leg: &LegInfo<'_>,
    now: Instant,
) -> Reach {
    // A terminal record is immutable — late/duplicate legs are ignored.
    if tasks
        .get(leg.task_id)
        .is_some_and(|rec| rec.state.is_terminal())
    {
        return Reach::Finished;
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
        return Reach::Unknown;
    };

    // Only the task's own counterparty may drive it. The frame is
    // signature-verified upstream, but a signature is not a party: `advance`
    // *derives* the sender from our own role, so without this an unchecked leg
    // is attributed to the counterparty by construction and any mesh member
    // could close, park, or beat a task they are not in.
    if rec.peer_key != *leg.peer_key {
        return Reach::Foreign;
    }

    // A leg that is not news must not move the state either: a question replayed after its
    // answer, a follow-up replayed behind a newer question, or a check-in delayed behind its
    // own artifact would otherwise undo what a later leg decided. A beat is liveness only.
    if !leg.mine && !leg.news && leg.kind != LegKind::Beat {
        return Reach::Live;
    }
    let accepted = advance(rec, leg.kind, leg.mine);
    // A task that this leg ended waits for no review and on no one.
    if rec.state.is_terminal() {
        rec.review = false;
        rec.waiting_user = false;
    }
    // Any leg of the peer shows that the peer is alive, accepted or not.
    rec.last_activity = now;
    if leg.mine {
        rec.last_sent = now;
    } else {
        rec.last_peer_activity = now;
    }
    if !accepted {
        return Reach::Ignored;
    }
    if leg.kind != LegKind::Beat && leg.news {
        rec.last_skill_activity = now;
        rec.last_notice = None;
        if leg.mine && rec.role == TaskRole::Receiver {
            rec.unanswered = None;
        }
    }
    Reach::Live
}

/// Where a leg landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reach {
    /// The task is open, and the leg came from its counterparty.
    Live,
    /// The task is over, so its record is frozen.
    Finished,
    /// The leg came from a peer that is not the task's counterparty.
    Foreign,
    /// We hold no record of the task.
    Unknown,
    /// The leg came from the counterparty, but the state machine has no transition for it in the
    /// state of the task. It shows that the peer is alive and is nothing else.
    Ignored,
}

/// The per-leg coarse transition (state + review + ball). Says whether the leg is one that the
/// state machine accepts in the state of the record: only an accepted leg is news, and only an
/// accepted leg moves the check-in clock. An illegal, out-of-order or misdirected leg is ignored
/// (the conservative, no-consensus rule). Direction matters: only the worker advances into
/// `working` / `input-required` / `rejected` / `failed` **and closes the task with `completed`**
/// (native A2A — the server drives its own task to terminal); the initiator only sends messages
/// (answers / approval / change requests). `canceled` is open to both (and to the daemon's own
/// timeout).
fn advance(rec: &mut TaskRecord, kind: LegKind, mine: bool) -> bool {
    let sender = if mine { rec.role } else { rec.role.opposite() };
    let from_worker = sender == TaskRole::Receiver;
    match kind {
        // A check-in: the worker commits (accept), resumes, or says that it works on.
        LegKind::Status(TaskState::Working)
            if from_worker
                && matches!(
                    rec.state,
                    TaskState::Submitted | TaskState::Working | TaskState::InputRequired
                ) =>
        {
            rec.state = TaskState::Working;
            rec.review = false;
            true
        }
        // The worker asks a question → the initiator owes the answer.
        LegKind::Status(TaskState::InputRequired)
            if from_worker
                && matches!(
                    rec.state,
                    TaskState::Submitted | TaskState::Working | TaskState::InputRequired
                ) =>
        {
            rec.state = TaskState::InputRequired;
            rec.review = false;
            true
        }
        // A message into the task (the initiator's answer / approval / change request, or the
        // worker's own note): flip the ball to whoever now owes the reply. An initiator's message
        // resolves an `input-required` park — the worker resumes and decides whether to complete
        // (approval) or rework (change); the skill reads the text.
        LegKind::Text => {
            if sender == TaskRole::Initiator && rec.state == TaskState::InputRequired {
                rec.state = TaskState::Working;
                rec.review = false;
            }
            true
        }
        // The result arrived → park for the initiator's review. From `Submitted` too: a worker may
        // run a trivial task without an explicit accept first. From `input-required` too: a worker
        // that was parked may send a corrected result, or ask another question, before any
        // follow-up of the initiator.
        LegKind::Artifact
            if from_worker
                && matches!(
                    rec.state,
                    TaskState::Submitted | TaskState::Working | TaskState::InputRequired
                ) =>
        {
            rec.state = TaskState::InputRequired;
            rec.review = true;
            true
        }
        // Native A2A: the worker (server) drives its own task to `completed`, after the
        // initiator's approval message. From `Submitted` too: the legs of the worker are ordered
        // by their seq, so a `completed` that arrives before the accept is the newest leg, and
        // the accept that comes after it is dropped. The worker's emit side asks for the accept
        // first; the receiver does not.
        LegKind::Status(TaskState::Completed)
            if from_worker
                && matches!(
                    rec.state,
                    TaskState::Submitted | TaskState::Working | TaskState::InputRequired
                ) =>
        {
            rec.state = TaskState::Completed;
            true
        }
        LegKind::Status(TaskState::Rejected)
            if from_worker && matches!(rec.state, TaskState::Submitted | TaskState::Working) =>
        {
            rec.state = TaskState::Rejected;
            true
        }
        LegKind::Status(TaskState::Failed) if from_worker => {
            rec.state = TaskState::Failed;
            true
        }
        LegKind::Status(TaskState::Canceled) => {
            rec.state = TaskState::Canceled;
            true
        }
        // `Offer` is handled at record creation, and a `Beat` is liveness only.
        LegKind::Offer | LegKind::Beat => true,
        // Every other combination lands here only when its guard above failed: an
        // out-of-order, backward or misdirected leg, conservatively ignored.
        LegKind::Artifact
        | LegKind::Status(
            TaskState::Submitted
            | TaskState::Working
            | TaskState::InputRequired
            | TaskState::Completed
            | TaskState::Rejected
            | TaskState::Failed
            | TaskState::AuthRequired
            | TaskState::Unspecified,
        ) => false,
    }
}

/// A terminal status that this node sends to a peer: the cancel of the sweep, or the end of a
/// task that the peer's beat shows it never got.
struct End<'caller> {
    task_id: &'caller TaskId,
    peer_key: &'caller AuthorKey,
    state: TaskState,
    /// Why, for a cancel of the sweep.
    reason: Option<&'caller str>,
    /// The seq that the leg carries; none for a task that this node holds no record of.
    seq: Option<u64>,
}

/// Send each of `ends`, so that the peer's task ends as ours did.
async fn send_ends(state: &EventLoopState, context: &HandlerCtx<'_>, ends: Vec<End<'_>>) {
    for end in ends {
        let mut metadata = serde_json::json!({});
        if let Some(reason) = end.reason {
            metadata[META_REASON] = reason.into();
        }
        if let Some(seq) = end.seq {
            metadata[META_SEQ] = seq.into();
        }
        let update = status_update_params::status_update(
            context.mesh,
            status_update_params::StatusUpdateParams::new(end.task_id, end.state)
                .with_metadata(metadata),
        );
        broadcast_status(state, context, end.peer_key, &update).await;
    }
}

/// The ends that this node owes now: for each task that a beat showed unfinished at the peer, the
/// terminal status of the task, with the seq of the leg that ended it. A task that this node no
/// longer holds, or did not end by a leg of its own, owes nothing.
fn owed_end_legs(
    tasks: &HashMap<TaskId, TaskRecord>,
    closed: &ClosedTasks,
    owed: &[(TaskId, AuthorKey)],
) -> Vec<(TaskId, AuthorKey, TaskState, u64)> {
    owed.iter()
        .filter_map(|(task_id, peer_key)| {
            let from_record = tasks
                .get(task_id)
                .filter(|rec| owes_end(rec))
                .map(|rec| (rec.state, rec.applied_seq));
            let from_reaped = closed
                .get(task_id)
                .filter(|reaped| reaped.owes_end)
                .map(|reaped| (reaped.state, reaped.seq));
            let (state, seq) = from_record.or(from_reaped)?;
            Some((task_id.clone(), *peer_key, state, seq))
        })
        .collect()
}

/// Send the ends that the app owes now, without waiting for the task sweep. The receive path calls
/// it after each frame that owed one.
pub(crate) async fn send_owed_ends(
    state: &EventLoopState,
    app: &mut A2aApp,
    context: &HandlerCtx<'_>,
) {
    let owed = app.take_owed_ends();
    let legs = owed_end_legs(&app.tasks, &app.closed_tasks, &owed);
    let ends: Vec<End<'_>> = legs
        .iter()
        .map(|(task_id, peer_key, ended, seq)| End {
            task_id,
            peer_key,
            state: *ended,
            reason: None,
            seq: Some(*seq),
        })
        .collect();
    send_ends(state, context, ends).await;
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
    // The ends that the receive path owes go out with the cancels of the sweep. A cancel of the
    // sweep is a new leg, so it takes the next seq.
    let owed = app.take_owed_ends();
    let owed_legs = owed_end_legs(&app.tasks, &app.closed_tasks, &owed);
    let mut ends: Vec<End<'_>> = Vec::new();
    for (task_id, peer_key) in &expired {
        let seq = app.tasks.get_mut(task_id).map(|rec| {
            rec.take_seq_applied(rec.seq.saturating_add(1));
            rec.applied_seq
        });
        ends.push(End {
            task_id,
            peer_key,
            state: TaskState::Canceled,
            reason: Some("timeout"),
            seq,
        });
    }
    ends.extend(owed_legs.iter().map(|(task_id, peer_key, ended, seq)| End {
        task_id,
        peer_key,
        state: *ended,
        reason: None,
        seq: Some(*seq),
    }));
    send_ends(state, context, ends).await;

    // A reaped task's offloaded blobs go with it — unlink their spool files
    // (the review window has long closed).
    let orphans = app.take_orphan_blobs();
    if let Some(server) = app.blob_server.as_ref() {
        for task_id in &reaped {
            server
                .evict_content(&ContentId::new(task_id.as_str()))
                .await;
        }
        for group in &orphans {
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
            let owes = owes_end(&rec);
            closed.insert(
                task_id.clone(),
                ClosedTask {
                    peer_key: rec.peer_key,
                    peer: rec.peer,
                    state: rec.state,
                    seq: rec.applied_seq,
                    owes_end: owes,
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
            rec.cancel_without_leg(now);
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
    /// `(task_id, peer, state, what the beat adds)` for one keepalive-due task.
    type KeepaliveDue = (TaskId, AuthorKey, TaskState, BeatExtras);

    let now = Instant::now();
    let cadence = Duration::from_secs(task_keepalive_secs());
    let quiet = app
        .tasks
        .iter()
        .filter(|(_, rec)| rec.should_keepalive(now, cadence));
    let due: Vec<KeepaliveDue> = quiet
        .map(|(task_id, rec)| {
            let extras = rec.beat_extras(now);
            (task_id.clone(), rec.peer_key, rec.state, extras)
        })
        .collect();

    for (task_id, peer_key, task_state, extras) in due {
        let update = status_update_params::status_update(
            context.mesh,
            status_update_params::StatusUpdateParams::new(&task_id, task_state)
                .with_metadata(status_update_params::beat_metadata_with_extras(extras)),
        );
        let sent = broadcast_status(state, context, &peer_key, &update).await;
        if sent && let Some(rec) = app.tasks.get_mut(&task_id) {
            let sent_at = Instant::now();
            rec.last_activity = sent_at;
            rec.last_sent = sent_at;
        }
    }
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
    peer_key: &AuthorKey,
    update: &super::TaskStatusUpdate,
) -> bool {
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
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{LazyLock, Mutex};
    use std::time::{Duration, Instant};

    use habilis_network::protocol::messages::Authored;
    use habilis_network::protocol::{AuthorKey, Identity, Message, Nickname, UnixSecs};

    use super::{
        IngestLegParams, LegInfo, LegKind, TaskRecord, TaskRole, TaskState, Unanswered, apply,
    };
    use crate::a2a::status_update_params::Progress;
    use crate::a2a::tuning::{TASK_STATUS_TEXT_MAX_CHARS, TASK_UNANSWERED_TEXT_MAX_CHARS};
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

    /// The seq of the next test leg. A leg built after another carries a higher one, as the legs
    /// of one worker do, and a test that wants another order names the seq itself.
    fn next_test_seq() -> u64 {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }

    /// A text of the worker that the cap did not cut.
    fn capped(text: &str) -> status_update_params::CappedText {
        status_update_params::CappedText {
            text: text.to_owned(),
            truncated: false,
        }
    }

    /// What the worker's task says when it is in `state` after `seq` legs, with nothing else set.
    fn snapshot_at(state: TaskState, seq: u64) -> super::Snapshot {
        super::Snapshot {
            state,
            seq: Some(seq),
            progress: None,
            text: None,
            waiting_user: false,
            review: false,
        }
    }

    /// `metadata` with the next test seq on it, unless it is a beat, which takes none, or it
    /// names a seq already.
    fn stamped(metadata: Option<serde_json::Value>) -> serde_json::Value {
        let mut metadata = metadata.unwrap_or_else(|| serde_json::json!({}));
        if metadata.get("mesh:beat").is_none() && metadata.get("mesh:seq").is_none() {
            metadata["mesh:seq"] = next_test_seq().into();
        }
        metadata
    }

    /// An inbound status frame for `tid()` in `state`, carrying `metadata` and the next seq.
    fn status_frame(state: TaskState, metadata: Option<serde_json::Value>) -> Message {
        raw_status_frame(state, Some(stamped(metadata)))
    }

    /// An inbound status frame for `tid()` in `state` with exactly `metadata`: no seq is added.
    fn raw_status_frame(state: TaskState, metadata: Option<serde_json::Value>) -> Message {
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
                    next_test_seq(),
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

    /// The RPC path of a follow-up reads the same sequence.
    #[test]
    fn a_replayed_follow_up_does_not_move_the_clock_on_the_rpc_path() {
        let start = Instant::now();
        let mut tasks = task_we_serve(start);
        let mut moved = Vec::new();
        for after in [10, 100] {
            let now = later(start, Duration::from_secs(after));
            receive_follow_up(&mut tasks, 1, now);
            moved.push(tasks[&tid()].last_skill_activity);
        }
        assert_eq!(
            moved[1], moved[0],
            "the second copy of the follow-up is a replay"
        );
    }

    /// A beat of the worker that repeats `percent` and the seq `seq` of its last leg.
    fn beat_with_progress(percent: u8, seq: u64) -> Message {
        let metadata =
            status_update_params::beat_metadata_with_extras(status_update_params::BeatExtras {
                progress: Some(progress_of(percent)),
                seq: Some(seq),
                ..Default::default()
            });
        status_frame(TaskState::Working, Some(metadata))
    }

    /// A beat of the worker carries the progress of its last check-in. When the beat overtakes
    /// that check-in, the sender shows the progress at once, and the check-in still wakes it
    /// when it arrives, because the beat does not touch what the wake compares against.
    #[test]
    fn a_beat_that_overtakes_its_check_in_does_not_eat_the_wake() {
        let mut tasks = working_task_we_opened(Instant::now());
        receive(&mut tasks, &checkin_leg("started", "10", false));
        let beat = beat_with_progress(80, 1_000);
        receive(&mut tasks, &beat);
        assert_eq!(
            tasks[&tid()].shown_progress,
            Some(progress_of(80)),
            "the beat repairs what the sender shows"
        );
        assert_eq!(
            tasks[&tid()].last_progress,
            Some(progress_of(10)),
            "the beat leaves the check-in base alone"
        );
        assert!(
            receive(&mut tasks, &checkin_leg("nearly there", "80", false)).quarter_moved,
            "the check-in that the beat overtook still wakes the sender"
        );
    }

    /// The worker's own record is the authority on its progress, and a beat that a relay
    /// delivers behind a newer one says nothing about it.
    #[test]
    fn only_a_current_beat_on_the_senders_record_sets_the_shown_progress() {
        let sent = UnixSecs::now();
        let beat = |percent: u8, seq: u64, at: UnixSecs| {
            let mut frame = beat_with_progress(percent, seq);
            frame.timestamp = at;
            frame
        };
        let start = Instant::now();
        let mut worker = task_we_serve(start);
        ingest_at(&mut worker, &beat(70, 3, sent), start);
        assert_eq!(
            worker[&tid()].shown_progress,
            None,
            "the worker does not take its progress from the sender's beat"
        );

        let mut sender = working_task_we_opened(start);
        ingest_at(
            &mut sender,
            &beat(60, 4, sent + Duration::from_secs(10)),
            start,
        );
        ingest_at(&mut sender, &beat(20, 3, sent), start);
        assert_eq!(
            sender[&tid()].shown_progress,
            Some(progress_of(60)),
            "a beat sent before the one already taken is not news"
        );
    }

    /// A question that a relay replays after its answer must not park the task again: the sender
    /// would stop the clock and never raise the alarm.
    #[test]
    fn a_replayed_question_does_not_park_a_task_that_was_answered() {
        let start = Instant::now();
        let mut tasks = HashMap::new();
        open_as_initiator(&mut tasks, start);
        receive(&mut tasks, &status_leg(false));
        let question = status_frame(TaskState::InputRequired, None);
        assert!(
            receive(&mut tasks, &question).news,
            "a question seen for the first time is worth surfacing"
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::InputRequired,
            "the worker waits for the answer"
        );
        super::note_follow_up_sent(
            &mut tasks,
            &tid(),
            &peer_key("calm-otter"),
            (2, later(start, Duration::from_secs(10))),
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Working,
            "the answer resumes it"
        );
        assert!(
            !receive(&mut tasks, &question).news,
            "the second copy of the question is a replay"
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Working,
            "a replayed question must not park the task again"
        );
    }

    /// A follow-up that a relay replays behind a newer question must not resume a worker that
    /// still waits for the answer to that question.
    #[test]
    fn a_replayed_follow_up_does_not_resume_a_worker_that_asked() {
        let start = Instant::now();
        let mut tasks = task_we_serve(start);
        let message = |registry: &mut HashMap<TaskId, TaskRecord>, seq: u64| {
            receive_follow_up(registry, seq, later(start, Duration::from_secs(10)))
        };
        assert!(
            message(&mut tasks, 1).news,
            "a follow-up seen for the first time is worth surfacing"
        );
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::InputRequired), true),
            later(start, Duration::from_secs(20)),
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::InputRequired,
            "the worker asked a question"
        );
        assert!(
            !message(&mut tasks, 1).news,
            "the second copy of the follow-up is a replay"
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::InputRequired,
            "a replayed follow-up is not the answer to the question"
        );
    }

    /// A check-in that a slow relay delivers behind the artifact that followed it is dropped
    /// whole: the later leg wins, and the late one neither surfaces nor moves the task.
    #[test]
    fn a_check_in_delayed_behind_its_artifact_is_dropped() {
        let start = Instant::now();
        let mut tasks = HashMap::new();
        open_as_initiator(&mut tasks, start);
        ingest_at(&mut tasks, &seq_leg(false, 1), start);
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Working,
            "the worker accepted"
        );
        ingest_at(&mut tasks, &artifact_leg(3), start);
        assert_eq!(
            tasks[&tid()].state,
            TaskState::InputRequired,
            "the result parks the task for review"
        );
        assert!(tasks[&tid()].review, "the task awaits the review");
        assert!(
            !receive(&mut tasks, &seq_leg(false, 2)).news,
            "a check-in sent before the artifact is late, so it stays off the surface"
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::InputRequired,
            "a check-in sent before the artifact cannot end the review"
        );
        assert!(tasks[&tid()].review, "the review stands");
        ingest_at(&mut tasks, &seq_leg(false, 4), start);
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Working,
            "a check-in sent after the artifact is the rework"
        );
    }

    /// The cost that the strict rule accepts: an artifact that a later leg overtook is dropped
    /// whole, and the review it would have asked for never starts. The worker sends the result
    /// again as a new leg, which applies like any other.
    #[test]
    fn an_artifact_delayed_behind_a_later_leg_is_dropped() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        ingest_at(&mut tasks, &seq_leg(false, 5), start);

        let late = receive(&mut tasks, &artifact_leg(4));

        assert!(!late.news, "an artifact behind a newer leg is not news");
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Working,
            "the dropped artifact does not park the task for review"
        );
        assert!(!tasks[&tid()].review, "no review was asked for");
        assert!(
            receive(&mut tasks, &artifact_leg(6)).news,
            "the second copy of the result is a new leg"
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::InputRequired,
            "the second copy of the result parks the task"
        );
    }

    /// A leg that is delivered twice is news once, whatever else it says.
    #[test]
    fn a_replayed_leg_is_dropped_whole() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        let frame = checkin_leg("built it", "40", true);
        assert!(
            receive(&mut tasks, &frame).news,
            "the first copy of a leg is news"
        );

        assert!(
            !receive(&mut tasks, &frame).news,
            "the second copy is a replay"
        );
    }

    /// A leg with no seq cannot be placed in the sequence, so it is dropped whole.
    #[test]
    fn a_leg_without_a_seq_is_dropped_whole() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        let unplaced = raw_status_frame(
            TaskState::Working,
            Some(serde_json::json!({ "mesh:waiting_user": true })),
        );

        assert!(
            !receive(&mut tasks, &unplaced).news,
            "a leg that carries no mesh:seq is not news"
        );
        assert!(
            !tasks[&tid()].waiting_user,
            "a leg that is dropped moves no flag"
        );
    }

    /// The two parties number their legs apart: a worker at seq 9 still takes the sender's seq 2,
    /// and a sender at seq 5 takes the worker's seq 1.
    #[test]
    fn the_follow_up_seq_and_the_worker_seq_are_separate_counters() {
        let start = Instant::now();
        let mut worker = task_we_serve(start);
        worker
            .get_mut(&tid())
            .expect("the test inserted this task earlier")
            .seq = 9;
        assert!(
            receive_follow_up(&mut worker, 2, start).news,
            "the worker's own count does not make the sender's follow-up late"
        );
        assert_eq!(
            worker[&tid()].peer_seq,
            2,
            "the worker took the follow-up in"
        );
        assert_eq!(worker[&tid()].seq, 9, "and its own count is unchanged");

        let mut sender = working_task_we_opened(start);
        sender
            .get_mut(&tid())
            .expect("the test inserted this task earlier")
            .seq = 5;
        assert!(
            receive(&mut sender, &seq_leg(false, 1)).news,
            "the sender's own count does not make the worker's first leg late"
        );
        assert_eq!(sender[&tid()].peer_seq, 1, "the sender took the leg in");
        assert_eq!(sender[&tid()].seq, 5, "and its own count is unchanged");
    }

    /// A leg takes its seq when it is built and its state when it was delivered. What the worker
    /// shows meanwhile, in a beat, a `GetTask` or an end, is the seq that goes with the state it
    /// holds: a peer that was told the seq of a leg in flight would count the leg as taken in,
    /// and drop it when it arrives.
    #[test]
    fn the_seq_that_is_shown_is_the_seq_of_the_state_that_is_held() {
        let start = Instant::now();
        let mut tasks = task_we_serve(start);
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Working), true),
            start,
        );
        super::take_seq(&mut tasks, &tid(), 1);
        assert_eq!(tasks[&tid()].seq, 1, "the leg in flight took its seq");
        assert_eq!(
            tasks[&tid()].applied_seq,
            0,
            "but the record does not hold its state yet, so it shows no seq for it"
        );

        let mesh = habilis_network::protocol::MeshId::from("test");
        let payload = status_update_params::status_update(
            &mesh,
            status_update_params::StatusUpdateParams {
                task_id: &tid(),
                state: TaskState::Working,
                note: None,
                metadata: Some(serde_json::json!({ "mesh:seq": 1 })),
            },
        );
        let body = serde_json::to_string(&payload).expect("a test value serializes to JSON");
        let own_leg = Message::new_app(
            &mesh,
            &Nickname::from("worker-bot"),
            habilis_network::protocol::AppFrameParams::new(
                wire::app_tag(wire::STATUS),
                Some(peer_key("calm-otter")),
                None,
                habilis_network::protocol::MessageBody::try_new(&body)
                    .expect("a test value serializes to JSON"),
            ),
        );
        super::ingest(
            &mut tasks,
            IngestLegParams {
                frame: &authored_by("worker-bot", &own_leg),
                task_id: &tid(),
                mine: true,
                now: start,
            },
        );
        assert_eq!(
            tasks[&tid()].applied_seq,
            1,
            "the leg was delivered: the record holds its state and shows its seq"
        );
    }

    /// A leg takes its seq when it is about to leave, and gives it back when it fails before it
    /// does; a later leg that took a higher one keeps its place.
    #[test]
    fn a_seq_is_consumed_by_a_leg_that_leaves() {
        let mut tasks = task_we_serve(Instant::now());
        assert_eq!(
            super::next_seq(&tasks, &tid()),
            Some(1),
            "the worker numbers its legs from 1"
        );
        assert_eq!(
            super::next_seq(&tasks, &tid()),
            Some(1),
            "asking for a seq consumes none"
        );
        super::take_seq(&mut tasks, &tid(), 1);
        assert_eq!(
            super::next_seq(&tasks, &tid()),
            Some(2),
            "after the first leg left, the counter stands at 2"
        );

        super::give_back_seq(&mut tasks, &tid(), 1);
        assert_eq!(
            super::next_seq(&tasks, &tid()),
            Some(1),
            "a leg that failed before it left gives its seq back"
        );

        super::take_seq(&mut tasks, &tid(), 1);
        super::take_seq(&mut tasks, &tid(), 2);
        super::give_back_seq(&mut tasks, &tid(), 1);
        assert_eq!(
            tasks[&tid()].seq,
            2,
            "a seq that a later leg passed stays taken"
        );
        assert!(
            super::next_seq(&tasks, &TaskId::random()).is_none(),
            "a task that we do not hold has no next seq"
        );
    }

    /// A beat of the worker carries the seq of its last leg and takes no place in the sequence: it
    /// cannot make a real leg that it overtook late.
    #[test]
    fn a_beat_never_advances_the_sequence() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        let mut metadata =
            status_update_params::beat_metadata_with_extras(status_update_params::BeatExtras {
                seq: Some(7),
                ..Default::default()
            });
        metadata["mesh:seq"] = 7.into();
        receive(
            &mut tasks,
            &status_frame(TaskState::Working, Some(metadata)),
        );
        assert_eq!(
            tasks[&tid()].peer_seq,
            0,
            "a beat does not count in the sequence"
        );

        assert!(
            receive(&mut tasks, &seq_leg(false, 7)).news,
            "the leg that the beat overtook still applies"
        );
    }

    /// The sender's cancel counts on the sender's own seq: the worker takes it in although the
    /// worker numbered many legs of its own.
    #[test]
    fn a_cancel_from_the_sender_counts_on_its_own_seq() {
        let start = Instant::now();
        let mut tasks = task_we_serve(start);
        {
            let rec = tasks
                .get_mut(&tid())
                .expect("the test inserted this task earlier");
            rec.seq = 40;
            rec.peer_seq = 1;
        }
        let cancel = status_frame(
            TaskState::Canceled,
            Some(serde_json::json!({ "mesh:seq": 2 })),
        );
        let applied = receive(&mut tasks, &cancel);
        assert!(
            applied.news,
            "the worker takes the cancel in: the sender's seq 2 follows its seq 1"
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Canceled,
            "the worker stops working a task that its sender canceled"
        );
    }

    /// A follow-up or a brief that a relay delivers twice is news only the first time, and a
    /// follow-up to a task that already closed is not news at all.
    #[test]
    fn a_replayed_message_and_a_message_to_a_closed_task_are_not_news() {
        let start = Instant::now();
        let mut tasks = task_we_serve(start);
        let message = |registry: &mut HashMap<TaskId, TaskRecord>, seq: u64| {
            receive_follow_up(registry, seq, later(start, Duration::from_secs(10)))
        };
        assert!(
            message(&mut tasks, 1).news,
            "the first copy of a message is worth surfacing"
        );
        assert!(!message(&mut tasks, 1).news, "the second copy is a replay");
        assert!(
            message(&mut tasks, 2).news,
            "another follow-up is news again"
        );
        tasks
            .get_mut(&tid())
            .expect("the test inserted this task earlier")
            .state = TaskState::Completed;
        assert!(
            !message(&mut tasks, 3).news,
            "a closed task has nothing to tell its worker"
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

    /// The reminder repeats the follow-up the worker has not answered, until any leg of its own
    /// answers it.
    #[test]
    fn the_reminder_carries_the_unanswered_text_until_answered() {
        let start = Instant::now();
        let mut tasks = task_we_serve(start);
        let unanswered = Unanswered {
            text: "which port?".to_owned(),
            truncated: false,
            has_file: false,
            brief: false,
        };
        tasks
            .get_mut(&tid())
            .expect("the test inserted this task earlier")
            .unanswered = Some(unanswered);
        let reminders = super::due_checkins(&mut tasks, CHECKIN, later(start, CHECKIN)).1;
        assert_eq!(
            reminders.first().and_then(|due| due.text.as_deref()),
            Some("which port?"),
            "the reminder repeats the follow-up"
        );
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Working), true),
            later(start, CHECKIN * 2),
        );
        assert_eq!(
            tasks[&tid()].unanswered,
            None,
            "a leg of the worker answers it"
        );
    }

    /// The text a reminder repeats is peer-controlled: control characters go, the newline stays,
    /// and the cap cuts it and says so. A file is named by a flag, not repeated.
    #[test]
    fn the_unanswered_text_is_cleaned_and_capped() {
        let part = |text: &str| crate::a2a::Part::text(text);
        let message = |parts: Vec<crate::a2a::Part>| {
            let mut message = crate::a2a::Message::text(crate::a2a::Role::User, "");
            message.parts = parts;
            message
        };
        let cleaned = Unanswered::of(&message(vec![part("a\u{7}b\nc\te"), part("d")]), false);
        assert_eq!(
            cleaned.text, "ab\nc e\nd",
            "the newline stays, a tab is a space and the bell goes"
        );
        assert!(!cleaned.truncated, "a short text is not cut");
        let padded = Unanswered::of(&message(vec![part("  \n go on \t\n ")]), false);
        assert_eq!(
            padded.text, "go on",
            "the whitespace around the text is trimmed, so it cannot use up the cap"
        );
        let blank = Unanswered::of(&message(vec![part("  \n\t ")]), false);
        assert!(
            blank.text.is_empty() && !blank.truncated,
            "a message of spaces says nothing"
        );
        assert!(!cleaned.has_file, "no file part");
        assert!(
            Unanswered::of(&message(vec![part("go")]), true).brief,
            "the flag says that the message opens the task"
        );

        let long = "x".repeat(TASK_UNANSWERED_TEXT_MAX_CHARS + 1);
        let capped = Unanswered::of(&message(vec![part(&long)]), false);
        assert_eq!(
            capped.text.chars().count(),
            TASK_UNANSWERED_TEXT_MAX_CHARS,
            "the cap holds"
        );
        assert!(capped.truncated, "the cut is reported");

        let exact = "y".repeat(TASK_UNANSWERED_TEXT_MAX_CHARS);
        assert!(
            !Unanswered::of(&message(vec![part(&exact)]), false).truncated,
            "a text of exactly the cap is whole"
        );

        let file = crate::a2a::Part {
            url: Some("blob:abc".to_owned()),
            ..Default::default()
        };
        assert!(
            Unanswered::of(&message(vec![file]), true).has_file,
            "a file part is flagged"
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
                receive(registry, &checkin_leg("step", percent, false)).quarter_moved
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
            !receive(&mut tasks, &beat).quarter_moved,
            "a beat never wakes"
        );
    }

    /// An artifact frame for `tid()` from the worker, with the seq `seq`.
    fn artifact_leg(seq: u64) -> Message {
        use habilis_network::protocol::{AppFrameParams, MeshId, MessageBody};

        let mesh = MeshId::from("test");
        let payload = status_update_params::artifact_update(&mesh, &tid(), "the result", seq);
        Message::new_app(
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
        )
    }

    /// A delivered result answers what the worker waited on, so a rework that then stalls
    /// raises an alarm that rings, not one filed under `waiting on user`.
    #[test]
    fn an_artifact_clears_what_the_worker_waited_on() {
        let first = UnixSecs::now();
        let mut tasks = working_task_we_opened(Instant::now());
        ingest_inbound(&mut tasks, &waiting_leg(true, first));
        assert!(tasks[&tid()].waiting_user, "the check-in names the user");
        ingest_inbound(&mut tasks, &artifact_leg(next_test_seq()));
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
            (2, later(start, CHECKIN)),
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

    /// A frame from a member who is not the task's worker is not even remembered: it could move
    /// the sequence or forge the beat that silences the alarm.
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
        assert_eq!(rec.peer_seq, 0, "a stranger's seq is not taken in");
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

    /// A check-in that a slow relay delivers after a newer one is dropped whole, so it cannot
    /// postpone the alarm.
    #[test]
    fn a_check_in_delivered_late_does_not_restart_the_clock() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        let newer = later(start, Duration::from_secs(1));
        ingest_at(&mut tasks, &seq_leg(false, 2), newer);
        ingest_at(&mut tasks, &seq_leg(false, 1), later(start, CHECKIN));
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

    /// A reconciled reply refreshes the reaper clocks but not the check-in clock, so a sender that
    /// probes more often than 2·X still gets its alarm; only a follow-up restarts it.
    #[test]
    fn a_reconciled_reply_does_not_delay_the_alarm() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        let probe = later(start, CHECKIN);
        super::reconcile_initiator(
            &mut tasks,
            super::ReconcileParams {
                task_id: &tid(),
                peer_key: &peer_key("calm-otter"),
                snapshot: snapshot_at(TaskState::Working, 9),
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

    /// A relay can deliver an old status leg hours late. Both the `--waiting-on-user`
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
            "a replayed --waiting-on-user leg of the same second must not set the flag again"
        );

        deliver(&mut tasks, &clear);
        ingest_inbound(&mut tasks, &waiting);
        assert!(
            !tasks[&tid()].waiting_user,
            "a replayed --waiting-on-user leg must not set the flag again"
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
    /// reaches the record of seen legs too.
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
            "the replayed --waiting-on-user leg must not set the flag again"
        );
    }

    /// A working status leg for `tid()` with the seq `seq`, and a waiting reason when one is
    /// given.
    fn seq_leg(waiting_user: bool, seq: u64) -> Message {
        let mut metadata = serde_json::json!({ "mesh:seq": seq });
        if waiting_user {
            metadata["mesh:waiting_user"] = true.into();
        }
        status_frame(TaskState::Working, Some(metadata))
    }

    /// Two legs sent one after the other: the clear is sent after the waiting leg, but arrives
    /// first. Its seq is higher, so the older waiting leg is dropped and does not restore its
    /// reason.
    #[test]
    fn a_waiting_leg_sent_before_a_clear_does_not_restore_it() {
        let mut tasks = working_task_we_opened(Instant::now());
        let waiting = seq_leg(true, 1);
        let clear = seq_leg(false, 2);

        ingest_inbound(&mut tasks, &clear);
        ingest_inbound(&mut tasks, &waiting);
        assert!(
            !tasks[&tid()].waiting_user,
            "the older waiting leg must not restore its reason after the clear"
        );
    }

    /// The seq decides, not the wire second: a clear that carries the higher seq wins although
    /// its clock puts it a second behind the waiting leg.
    #[test]
    fn a_higher_seq_wins_over_a_later_wire_second() {
        let at = UnixSecs::now();
        let mut tasks = working_task_we_opened(Instant::now());
        let mut waiting = seq_leg(true, 1);
        waiting.timestamp = at + Duration::from_secs(1);
        ingest_inbound(&mut tasks, &waiting);
        assert!(
            tasks[&tid()].waiting_user,
            "the waiting question is the one asked"
        );

        let mut clear = seq_leg(false, 2);
        clear.timestamp = at;
        ingest_inbound(&mut tasks, &clear);
        assert!(
            !tasks[&tid()].waiting_user,
            "the clear with the higher seq must win"
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

    /// A finished task is frozen: a late `--waiting-on-user` leg sets nothing.
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
            !receive(&mut tasks, &late).news,
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
            !receive_with(&mut tasks, &reaped_ids, &tid(), &late).news,
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
                    seq: 0,
                    owes_end: false,
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
        assert!(
            receive(&mut tasks, &working).news,
            "the first copy of a leg is news"
        );
        assert!(
            !receive(&mut tasks, &working).news,
            "the second copy of the same leg is a replay"
        );
    }

    /// A new leg on a live task surfaces, and a leg for a task we hold no record of does not: the
    /// initiator opened its record at dispatch, so such a leg is for a task that is not ours.
    #[test]
    fn a_new_leg_surfaces_and_a_leg_for_an_unknown_task_does_not() {
        let now = Instant::now();
        let mut tasks = HashMap::new();
        open_as_initiator(&mut tasks, now);
        receive(&mut tasks, &status_leg(false));
        let fresh = status_leg(false);
        assert!(
            receive(&mut tasks, &fresh).news,
            "a live task must keep surfacing its new legs"
        );
        let unknown = TaskId::from("660e8400-e29b-41d4-a716-446655440000");
        assert!(
            !receive_with(&mut tasks, &closed(), &unknown, &fresh).news,
            "no record means no task of ours, so the leg is not news"
        );
        assert!(
            !tasks.contains_key(&unknown),
            "a leg for an unknown task opens no record"
        );
    }

    /// Receive `frame` as the worker's, with `closed` as the list of reaped tasks.
    fn receive_with(
        tasks: &mut HashMap<TaskId, TaskRecord>,
        closed: &super::ClosedTasks,
        task_id: &TaskId,
        frame: &Message,
    ) -> super::Applied {
        super::apply_leg(
            tasks,
            closed,
            super::Leg {
                task_id,
                peer_key: &peer_key("calm-otter"),
                peer: Some(&Nickname::from("calm-otter")),
                source: super::LegSource::Frame(&authored_by("calm-otter", frame)),
                mine: false,
                now: Instant::now(),
            },
        )
    }

    /// Receive the follow-up message with `seq` from the worker's peer at `now`.
    fn receive_follow_up(
        tasks: &mut HashMap<TaskId, TaskRecord>,
        seq: u64,
        now: Instant,
    ) -> super::Applied {
        super::apply_leg(
            tasks,
            &closed(),
            super::Leg {
                task_id: &tid(),
                peer_key: &peer_key("calm-otter"),
                peer: Some(&Nickname::from("calm-otter")),
                source: super::LegSource::Message {
                    seq,
                    opens_task: false,
                },
                mine: false,
                now,
            },
        )
    }

    /// Receive `frame` as the worker's, for `tid()`, with no task reaped.
    fn receive(tasks: &mut HashMap<TaskId, TaskRecord>, frame: &Message) -> super::Applied {
        receive_with(tasks, &closed(), &tid(), frame)
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
            news: true,
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
            news: true,
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
                    news: true,
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

    /// An RPC `Task` snapshot is reconciled only from the task's own worker.
    ///
    /// The receive path already proves the response answers a call we made to
    /// that peer, but a call to *anyone* was enough: pre-fix, one reply naming
    /// another peer's task id drove it terminal (`rec.peer` still read the
    /// honest worker, so nothing recorded who killed it), and a non-terminal
    /// snapshot refreshed `last_activity` unconditionally, which held any of
    /// our tasks off the reaper for as long as the caller kept answering.
    #[test]
    fn a_returned_task_is_reconciled_only_from_its_own_worker() {
        let now = Instant::now();
        let mut tasks = HashMap::new();
        open_as_initiator(&mut tasks, now);
        apply(
            &mut tasks,
            &leg(LegKind::Status(TaskState::Working), false),
            now,
        );
        let before = tasks[&tid()].last_activity;

        let reconcile = |registry: &mut HashMap<TaskId, TaskRecord>, peer: &str, state, when| {
            super::reconcile_initiator(
                registry,
                super::ReconcileParams {
                    task_id: &tid(),
                    peer_key: &peer_key(peer),
                    snapshot: snapshot_at(state, 9),
                    now: when,
                },
            );
        };

        let later = now
            .checked_add(Duration::from_mins(5))
            .expect("the test instant cannot overflow");
        reconcile(&mut tasks, "wire-thistle", TaskState::Failed, later);
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

        // The real worker's snapshot is still reconciled.
        reconcile(&mut tasks, "calm-otter", TaskState::Failed, later);
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
                news: true,
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
            news: true,
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
        let payload = status_update_params::artifact_update(&mesh, &task_id, "result text", 1);
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

    /// The legs of a worker are ordered by their seq, so a `completed` that arrives before the
    /// accept is the newest leg: the task ends completed, and the accept that comes after it is
    /// dropped. A decline is a different leg, and lands as before.
    #[test]
    fn a_completed_leg_that_overtakes_the_accept_closes_the_task() {
        let start = Instant::now();
        let mut tasks = HashMap::new();
        open_as_initiator(&mut tasks, start);
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Submitted,
            "the worker has not answered yet"
        );

        let done = status_frame(
            TaskState::Completed,
            Some(serde_json::json!({ "mesh:seq": 3 })),
        );
        assert!(
            receive(&mut tasks, &done).news,
            "the newest leg applies, whatever came before it"
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Completed,
            "the receiver does not demand an accept first"
        );
        assert!(
            !receive(&mut tasks, &seq_leg(false, 2)).news,
            "the accept that comes after the result is dropped"
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::Completed,
            "a dropped accept does not reopen the task"
        );

        let mut declined = HashMap::new();
        open_as_initiator(&mut declined, start);
        apply(
            &mut declined,
            &leg(LegKind::Status(TaskState::Failed), false),
            start,
        );
        assert_eq!(
            declined[&tid()].state,
            TaskState::Failed,
            "a decline is a different leg from a close, so it must still land"
        );
    }

    /// A beat that a relay delivers late must not show an older progress than the check-in it
    /// overtook, and the check-in it overtook must not undo what a newer beat showed.
    #[test]
    fn the_progress_shown_comes_from_the_highest_seq() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        let beat = |seq: u64, percent: u8| {
            let mut metadata =
                status_update_params::beat_metadata_with_extras(status_update_params::BeatExtras {
                    progress: Some(progress_of(percent)),
                    seq: Some(seq),
                    ..Default::default()
                });
            metadata["mesh:seq"] = seq.into();
            status_frame(TaskState::Working, Some(metadata))
        };
        let checkin = |seq: u64, percent: &str| {
            status_frame(
                TaskState::Working,
                Some(status_update_params::status_metadata(
                    false,
                    Some(percent.parse().expect("the test passes a percentage")),
                    seq,
                )),
            )
        };

        receive(&mut tasks, &checkin(5, "60"));
        receive(&mut tasks, &beat(3, 20));
        assert_eq!(
            tasks[&tid()].shown_progress,
            Some(progress_of(60)),
            "a delayed beat at 20 % must not overwrite the check-in at 60 %"
        );

        receive(&mut tasks, &beat(6, 70));
        assert_eq!(
            tasks[&tid()].shown_progress,
            Some(progress_of(70)),
            "a beat of a higher seq repairs a check-in that never arrived"
        );
        receive(&mut tasks, &checkin(6, "65"));
        assert_eq!(
            tasks[&tid()].shown_progress,
            Some(progress_of(70)),
            "a check-in that a newer beat overtook does not undo what the beat showed"
        );
        assert_eq!(
            tasks[&tid()].last_progress,
            Some(progress_of(65)),
            "the wake still compares against the check-ins"
        );
        assert_eq!(
            tasks[&tid()].peer_seq,
            6,
            "a beat never advances the sequence of legs"
        );
    }

    /// A task that ends waits on no one and asks for no review, whichever leg or snapshot
    /// ended it.
    #[test]
    fn a_task_that_ends_clears_the_wait_and_the_review() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        {
            let rec = tasks
                .get_mut(&tid())
                .expect("the test inserted this task earlier");
            rec.waiting_user = true;
            rec.state = TaskState::InputRequired;
            rec.review = true;
        }
        receive(&mut tasks, &status_frame(TaskState::Completed, None));
        let rec = &tasks[&tid()];
        assert_eq!(
            rec.state,
            TaskState::Completed,
            "the worker closed the task"
        );
        assert!(!rec.review, "the review flag goes with the end");
        assert!(!rec.waiting_user, "the wait on the user goes with the end");

        let mut snapshot = working_task_we_opened(start);
        snapshot
            .get_mut(&tid())
            .expect("the test inserted this task earlier")
            .waiting_user = true;
        super::reconcile_initiator(
            &mut snapshot,
            super::ReconcileParams {
                task_id: &tid(),
                peer_key: &peer_key("calm-otter"),
                snapshot: snapshot_at(TaskState::Failed, 9),
                now: start,
            },
        );
        assert!(
            !snapshot[&tid()].waiting_user,
            "a terminal snapshot clears the wait as well"
        );
    }

    /// A message of nothing but whitespace is no text: the reminder repeats none.
    #[test]
    fn a_blank_unanswered_text_is_not_repeated() {
        let start = Instant::now();
        let mut tasks = task_we_serve(start);
        tasks
            .get_mut(&tid())
            .expect("the test inserted this task earlier")
            .unanswered = Some(Unanswered {
            text: "  \n \t ".to_owned(),
            truncated: false,
            has_file: false,
            brief: true,
        });
        let reminders = super::due_checkins(&mut tasks, CHECKIN, later(start, CHECKIN)).1;
        assert_eq!(
            reminders.first().map(|due| due.text.clone()),
            Some(None),
            "a blank text would be an empty body"
        );
    }

    /// A worker that was parked for review may send a corrected result, or ask another question,
    /// before any follow-up: the sender takes it in, and the review stands.
    #[test]
    fn a_worker_in_review_can_send_another_result_or_question() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        assert!(
            receive(&mut tasks, &artifact_leg(next_test_seq())).news,
            "the first result parks the task"
        );
        assert_eq!(
            tasks[&tid()].state,
            TaskState::InputRequired,
            "the task waits for the review"
        );

        assert!(
            receive(&mut tasks, &artifact_leg(next_test_seq())).news,
            "a corrected result surfaces, though no follow-up came between the two"
        );
        assert!(tasks[&tid()].review, "the review stands for the new result");
        assert!(
            receive(&mut tasks, &status_frame(TaskState::InputRequired, None)).news,
            "a question after the result surfaces too"
        );
        assert!(
            !tasks[&tid()].review,
            "a question leaves nothing to approve"
        );
    }

    /// A clock that runs a minute ahead is healthy even when the idle timeout is a few seconds,
    /// and one far ahead is not; a clock behind by more than the timeout is stale.
    #[test]
    fn the_clock_tolerance_has_a_floor() {
        let now = UnixSecs::now();
        let ahead = |secs: u64| now + Duration::from_secs(secs);
        assert_eq!(
            super::skew_between(ahead(60), now, 3),
            None,
            "a minute ahead is inside the floor, whatever the idle timeout"
        );
        assert_eq!(
            super::skew_between(ahead(600), now, 3),
            Some(super::ClockSkew::Ahead),
            "ten minutes ahead is beyond it"
        );
        assert_eq!(
            super::skew_between(now - Duration::from_secs(10), now, 3),
            Some(super::ClockSkew::Behind),
            "older than the idle timeout is stale, as before"
        );
    }

    /// A beat of the worker that repeats a seq above the one taken in shows a leg that never
    /// arrived: the sender asks the worker for its task, once while the ask is in flight, and
    /// never for a seq it already holds.
    #[test]
    fn a_beat_that_shows_a_lost_leg_asks_once() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        tasks
            .get_mut(&tid())
            .expect("the test inserted this task earlier")
            .peer_seq = 2;
        let beat = beat_with_progress(10, 5);

        assert!(
            receive(&mut tasks, &beat).beat == Some(super::BeatAction::Ask),
            "a beat at seq 5 shows legs after seq 2"
        );
        assert!(
            receive(&mut tasks, &beat).beat != Some(super::BeatAction::Ask),
            "the same seq is asked about once"
        );
        assert!(
            receive(&mut tasks, &beat_with_progress(10, 6)).beat != Some(super::BeatAction::Ask),
            "a higher seq waits for the ask in flight"
        );
        assert!(
            receive(&mut tasks, &beat_with_progress(10, 2)).beat != Some(super::BeatAction::Ask),
            "a beat at the seq that is held shows nothing lost"
        );
        assert_eq!(
            tasks[&tid()].peer_seq,
            2,
            "the question alone does not move the sequence"
        );
    }

    /// Only the sender of an open task asks: a worker takes nothing from a beat of its sender, and
    /// a finished task needs no question.
    #[test]
    fn only_the_sender_of_an_open_task_asks_for_a_lost_leg() {
        let start = Instant::now();
        let mut worker = task_we_serve(start);
        assert!(
            receive(&mut worker, &beat_with_progress(10, 9)).beat != Some(super::BeatAction::Ask),
            "a worker asks no one"
        );
        let mut ended = working_task_we_opened(start);
        ended
            .get_mut(&tid())
            .expect("the test inserted this task earlier")
            .close(TaskState::Failed, start);
        assert!(
            receive(&mut ended, &beat_with_progress(10, 9)).beat != Some(super::BeatAction::Ask),
            "a task that is over asks nothing"
        );
    }

    /// A snapshot that is ahead of the record stands for the legs that were lost: its state,
    /// progress and text are taken in, and its seq counts as taken in. One that is not ahead
    /// changes nothing but the reaper's clocks.
    #[test]
    fn a_snapshot_ahead_of_the_record_stands_for_the_lost_legs() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        let snapshot = |seq: u64, percent: u8, text: &str| super::Snapshot {
            progress: Some(progress_of(percent)),
            text: Some(capped(text)),
            ..snapshot_at(TaskState::Working, seq)
        };
        let reconcile = |registry: &mut HashMap<TaskId, TaskRecord>, stated| {
            super::reconcile_initiator(
                registry,
                super::ReconcileParams {
                    task_id: &tid(),
                    peer_key: &peer_key("calm-otter"),
                    snapshot: stated,
                    now: start,
                },
            )
        };

        reconcile(&mut tasks, snapshot(7, 60, "built it"));
        assert_eq!(
            tasks[&tid()].peer_seq,
            7,
            "the seq of the snapshot is taken in"
        );
        assert_eq!(
            tasks[&tid()].shown_progress,
            Some(progress_of(60)),
            "and the progress that it states is what the sender shows"
        );
        assert_eq!(
            tasks[&tid()].status_text.as_deref(),
            Some("built it"),
            "and its text is kept"
        );

        reconcile(&mut tasks, snapshot(7, 10, "older"));
        assert_eq!(
            tasks[&tid()].status_text.as_deref(),
            Some("built it"),
            "a snapshot at the seq that is held changes nothing"
        );
        assert!(
            !receive(&mut tasks, &seq_leg(false, 7)).news,
            "a leg that the snapshot already stands for is late"
        );
    }

    /// A snapshot that moves the task into `input-required` says that a question was lost, and the
    /// sender is told; a snapshot that leaves the task where it was, or that moves it anywhere
    /// else, is not a gap.
    #[test]
    fn only_a_lost_question_or_end_is_a_gap() {
        let start = Instant::now();
        let reconcile = |registry: &mut HashMap<TaskId, TaskRecord>, stated| {
            super::reconcile_initiator(
                registry,
                super::ReconcileParams {
                    task_id: &tid(),
                    peer_key: &peer_key("calm-otter"),
                    snapshot: stated,
                    now: start,
                },
            )
        };
        let question = |seq: u64| super::Snapshot {
            text: Some(capped("which port?")),
            ..snapshot_at(TaskState::InputRequired, seq)
        };

        let mut tasks = working_task_we_opened(start);
        let gap = reconcile(&mut tasks, question(4))
            .expect("the snapshot moved the task into a question that was lost");
        assert_eq!(
            gap.text.as_deref(),
            Some("which port?"),
            "the gap carries the question"
        );
        assert_eq!(
            gap.peer,
            Nickname::from("calm-otter"),
            "and the worker that asked it"
        );
        assert!(
            reconcile(&mut tasks, question(5)).is_some(),
            "a second question that was lost, on a task parked already, is a gap too"
        );

        let mut other = working_task_we_opened(start);
        assert!(
            reconcile(&mut other, snapshot_at(TaskState::Working, 4)).is_none(),
            "a task that works on has lost no question"
        );
        let ended = reconcile(&mut other, snapshot_at(TaskState::Completed, 5))
            .expect("a task that ended has lost its end, and the agent hears of it");
        assert_eq!(ended.state, TaskState::Completed, "the notice is that end");
        assert_eq!(
            other[&tid()].state,
            TaskState::Completed,
            "the snapshot did end the task"
        );
    }

    /// A beat of the peer on a task that this node ended by a leg of its own shows that the peer
    /// never got that leg: the end is sent again. A task that the peer ended is not announced.
    #[test]
    fn a_beat_on_a_task_that_we_ended_asks_for_the_end_again() {
        let start = Instant::now();
        let beat = beat_with_progress(10, 3);
        let mut worker = task_we_serve(start);
        worker
            .get_mut(&tid())
            .expect("the test inserted this task earlier")
            .close(TaskState::Completed, start);
        assert!(
            receive(&mut worker, &beat).beat == Some(super::BeatAction::Resend),
            "a worker that completed the task sends the end again"
        );

        let mut sender = working_task_we_opened(start);
        sender
            .get_mut(&tid())
            .expect("the test inserted this task earlier")
            .close(TaskState::Completed, start);
        assert!(
            receive(&mut sender, &beat).beat != Some(super::BeatAction::Resend),
            "the sender was told that the task completed, and has nothing to announce"
        );

        let mut canceled = working_task_we_opened(start);
        canceled
            .get_mut(&tid())
            .expect("the test inserted this task earlier")
            .close(TaskState::Canceled, start);
        assert!(
            receive(&mut canceled, &beat).beat == Some(super::BeatAction::Resend),
            "either side that canceled the task sends the cancel again"
        );
    }

    /// The end of a reaped task is sent again as well, with the seq of the leg that ended it, so
    /// that a peer that never got it applies it and a peer that did drops it.
    #[test]
    fn the_end_of_a_reaped_task_keeps_its_seq() {
        let start = Instant::now();
        let mut tasks = HashMap::new();
        let mut reaped = closed();
        reaped.insert(
            tid(),
            super::ClosedTask {
                peer_key: peer_key("calm-otter"),
                peer: Nickname::from("calm-otter"),
                state: TaskState::Completed,
                seq: 8,
                owes_end: true,
            },
        );
        let applied = receive_with(&mut tasks, &reaped, &tid(), &beat_with_progress(10, 3));
        assert!(
            applied.beat == Some(super::BeatAction::Resend),
            "a beat on a reaped task of ours asks for its end"
        );

        let owed = vec![(tid(), peer_key("calm-otter"))];
        let legs = super::owed_end_legs(&tasks, &reaped, &owed);
        assert_eq!(
            legs,
            vec![(tid(), peer_key("calm-otter"), TaskState::Completed, 8)],
            "the end repeats the leg that ended the task, seq included"
        );
        let _ = start;
    }

    /// A leg in flight has taken its seq and not its state yet. Everything that the record shows
    /// to the peer, which reads the seq with the state it goes with, shows the seq of the state:
    /// the beat, the answer to `GetTask`, the end that is owed, and the closed record.
    #[test]
    fn every_seq_the_record_shows_is_that_of_the_state_it_holds() {
        let start = Instant::now();
        let mut tasks = task_we_serve(start);
        {
            let rec = tasks
                .get_mut(&tid())
                .expect("the test inserted this task earlier");
            rec.seq = 3;
            rec.applied_seq = 2;
        }

        assert_eq!(
            tasks[&tid()].beat_extras(start).seq,
            Some(2),
            "the beat shows the seq of the state held"
        );
        let mesh = habilis_network::protocol::MeshId::from("test");
        let object = rpc_error::task_object(&tid(), &tasks[&tid()], &mesh);
        assert_eq!(
            object["metadata"]["mesh:seq"], 2,
            "GetTask shows the seq of the state held"
        );

        tasks
            .get_mut(&tid())
            .expect("the test inserted this task earlier")
            .close(TaskState::Completed, start);
        let owed = vec![(tid(), peer_key("calm-otter"))];
        assert_eq!(
            super::owed_end_legs(&tasks, &closed(), &owed),
            vec![(tid(), peer_key("calm-otter"), TaskState::Completed, 2)],
            "the end that is owed shows the seq of the state held"
        );

        let mut reaped = closed();
        let timeout = Duration::from_secs(1);
        super::sweep_registry(&mut tasks, &mut reaped, later(start, timeout * 2), timeout);
        assert_eq!(
            reaped.get(&tid()).map(|task| task.seq),
            Some(2),
            "the closed record keeps the seq of the state held"
        );
    }

    /// A beat at a seq below the one held shows nothing: the worker numbers a beat with the seq of
    /// the last leg whose state it holds, and a leg in flight may reach this node first. The task
    /// goes on, and no question is asked.
    #[test]
    fn a_beat_at_a_seq_below_the_one_held_shows_nothing() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        tasks
            .get_mut(&tid())
            .expect("the test inserted this task earlier")
            .peer_seq = 5;

        let applied = receive(&mut tasks, &beat_with_progress(10, 2));
        assert_eq!(applied.beat, None, "the beat asks and ends nothing");
        assert_eq!(tasks[&tid()].state, TaskState::Working, "the task goes on");
        assert_eq!(tasks[&tid()].peer_seq, 5, "the seq held does not move back");
        assert_eq!(tasks[&tid()].asked_at, None, "and no ask is marked");
    }

    /// An ask whose reply never came (the call timed out, or its reply was lost) lapses after one
    /// call timeout, so a later beat that shows the same missing leg asks again. An ask that never
    /// left is forgotten at once.
    #[test]
    fn an_ask_that_got_no_reply_is_asked_again_once_it_lapses() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        let rec = tasks
            .get_mut(&tid())
            .expect("the test inserted this task earlier");
        rec.peer_seq = 2;
        assert!(
            super::asks_for_repair(rec, Some(5), start),
            "the first beat at seq 5 asks"
        );
        assert!(
            !super::asks_for_repair(rec, Some(5), start + Duration::from_secs(1)),
            "the ask is still waiting for its reply"
        );
        let lapsed = start + Duration::from_secs(tuning::CALL_TIMEOUT_SECS);
        assert!(
            super::asks_for_repair(rec, Some(5), lapsed),
            "an ask with no reply after a call timeout is asked again"
        );
        assert!(
            !super::asks_for_repair(rec, Some(5), lapsed),
            "and then waits again"
        );
        super::forget_ask(&mut tasks, &tid());
        let forgotten = tasks
            .get_mut(&tid())
            .expect("the test inserted this task earlier");
        assert!(
            super::asks_for_repair(forgotten, Some(5), lapsed),
            "an ask that never left is asked again by the next beat"
        );
    }

    /// A worker that beats at ever higher seqs and never answers costs one ask, not one per beat:
    /// the waiters of the node are shared by every call, so one task must not fill them. The
    /// reply clears the ask, and a later beat that still shows a missing leg asks again.
    #[test]
    fn a_task_has_one_ask_in_flight() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        let rec = tasks
            .get_mut(&tid())
            .expect("the test inserted this task earlier");
        let asks = (1..=64)
            .filter(|seq| super::asks_for_repair(rec, Some(*seq), start))
            .count();
        assert_eq!(
            asks, 1,
            "beats at 64 seqs cost one ask while it is in flight"
        );

        super::reconcile_initiator(
            &mut tasks,
            super::ReconcileParams {
                task_id: &tid(),
                peer_key: &peer_key("calm-otter"),
                snapshot: snapshot_at(TaskState::Working, 1),
                now: start,
            },
        );
        let answered = tasks
            .get_mut(&tid())
            .expect("the test inserted this task earlier");
        assert!(
            super::asks_for_repair(answered, Some(9), start),
            "the reply cleared the ask, so a beat that shows a leg beyond it asks again"
        );
    }

    /// A worker already in `input-required` can send another question or a corrected result. A
    /// snapshot that shows it, after the leg was lost, is a gap like any other.
    #[test]
    fn a_lost_second_question_is_a_gap() {
        let start = Instant::now();
        let mut parked = working_task_we_opened(start);
        let reconcile = |registry: &mut HashMap<TaskId, TaskRecord>, seq: u64| {
            super::reconcile_initiator(
                registry,
                super::ReconcileParams {
                    task_id: &tid(),
                    peer_key: &peer_key("calm-otter"),
                    snapshot: snapshot_at(TaskState::InputRequired, seq),
                    now: start,
                },
            )
        };
        assert!(
            reconcile(&mut parked, 2).is_some(),
            "the first question that was lost is a gap"
        );
        assert!(
            reconcile(&mut parked, 4).is_some(),
            "so is a second one, though the task was already parked"
        );
        assert!(
            reconcile(&mut parked, 4).is_none(),
            "a snapshot that shows nothing new is no gap"
        );
    }

    /// A snapshot that ends the task, or that parks it, raises a notice that carries what the
    /// worker said; one that waits on a result to review carries no text, because the last words
    /// are an older check-in.
    #[test]
    fn a_snapshot_that_moves_the_task_raises_a_notice() {
        let start = Instant::now();
        let reconcile = |tasks: &mut HashMap<TaskId, TaskRecord>, stated| {
            super::reconcile_initiator(
                tasks,
                super::ReconcileParams {
                    task_id: &tid(),
                    peer_key: &peer_key("calm-otter"),
                    snapshot: stated,
                    now: start,
                },
            )
        };
        let mut done = working_task_we_opened(start);
        let ended = super::Snapshot {
            text: Some(status_update_params::CappedText {
                text: "all\nfine".to_owned(),
                truncated: true,
            }),
            ..snapshot_at(TaskState::Completed, 6)
        };
        let notice = reconcile(&mut done, ended).expect("an end that was lost is told");
        assert_eq!(
            notice.state,
            TaskState::Completed,
            "the notice names the end"
        );
        assert_eq!(
            notice.text.as_deref(),
            Some("all\nfine"),
            "newlines are kept"
        );
        assert!(notice.truncated, "and the cut is reported");
        assert_eq!(
            done[&tid()].state,
            TaskState::Completed,
            "the record closed"
        );

        let mut parked = working_task_we_opened(start);
        let result = super::Snapshot {
            text: Some(capped("an older check-in")),
            review: true,
            ..snapshot_at(TaskState::InputRequired, 7)
        };
        let in_review = reconcile(&mut parked, result).expect("a lost result is told");
        assert!(
            in_review.review,
            "the notice says the task waits on a result"
        );
        assert_eq!(
            in_review.text, None,
            "the older check-in is not passed off as it"
        );
    }

    /// A task that ends with no leg of ours (the peer left) takes the next seq, so that the end
    /// sent again later is no repeat of a seq that the peer may hold.
    #[test]
    fn an_end_with_no_leg_takes_the_next_seq() {
        let start = Instant::now();
        let mut left = working_task_we_opened(start);
        left.get_mut(&tid())
            .expect("the test inserted this task earlier")
            .seq = 3;
        let out = crate::emission::Output::silent();
        super::fail_tasks_for_departed_peer(&mut left, &peer_key("calm-otter"), &out);
        assert_eq!(left[&tid()].seq, 4, "an end by a departure takes one too");
    }

    /// A relay can replay the last beat of the worker, even within its second: the replay must not
    /// refresh the age that the beat states, or the alarm would never ring.
    #[test]
    fn a_replayed_beat_does_not_refresh_the_beat_age() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        let beat = beat_with_progress(20, 3);

        ingest_at(&mut tasks, &beat, start);
        let first = tasks[&tid()].peer_beat.expect("the first beat is taken").at;
        ingest_at(&mut tasks, &beat, later(start, Duration::from_secs(60)));

        assert_eq!(
            tasks[&tid()].peer_beat.expect("the beat stays").at,
            first,
            "a beat of the same second is a replay, so the held beat stays"
        );
    }

    /// A beat that claims to come from far ahead of our clock would pin the order of the beats
    /// after it, so it is ignored like one that is far behind.
    #[test]
    fn a_beat_from_the_far_future_is_ignored() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        let mut ahead = beat_with_progress(90, 9);
        ahead.timestamp = UnixSecs::now() + Duration::from_secs(tuning::task_timeout_secs() * 4);
        ingest_at(&mut tasks, &ahead, start);
        assert!(
            tasks[&tid()].peer_beat.is_none(),
            "a beat from the future proves nothing about the peer now"
        );

        ingest_at(&mut tasks, &beat_with_progress(30, 10), start);
        assert!(
            tasks[&tid()].peer_beat.is_some(),
            "a beat of the right time is still taken after it"
        );
    }

    /// A leg that the state machine has no transition for shows that the peer is alive and is
    /// nothing else: it is no news, and it cannot postpone the alarm.
    #[test]
    fn an_illegal_leg_does_not_reset_the_check_in_clock() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        let bare = status_frame(TaskState::Submitted, None);

        let applied = {
            let now = later(start, CHECKIN);
            ingest_at(&mut tasks, &bare, now);
            receive(&mut tasks, &bare)
        };

        assert!(!applied.news, "a status that no worker may send is no news");
        assert_eq!(
            tasks[&tid()].last_skill_activity,
            start,
            "the check-in clock stays where the last accepted leg put it"
        );
        assert_eq!(
            tasks[&tid()].peer_seq,
            0,
            "a leg that is not accepted takes no seq in"
        );
        assert!(
            tasks[&tid()].last_peer_activity > start,
            "but the peer did show that it is alive"
        );
    }

    /// A reaped task is over: its opening cannot mint a record again.
    #[test]
    fn an_opening_for_a_reaped_task_mints_no_record() {
        let mut tasks = HashMap::new();
        let mut reaped = closed();
        reaped.insert(
            tid(),
            super::ClosedTask {
                peer_key: peer_key("calm-otter"),
                peer: Nickname::from("calm-otter"),
                state: TaskState::Completed,
                seq: 0,
                owes_end: false,
            },
        );

        let applied = super::apply_leg(
            &mut tasks,
            &reaped,
            super::Leg {
                task_id: &tid(),
                peer_key: &peer_key("calm-otter"),
                peer: Some(&Nickname::from("calm-otter")),
                source: super::LegSource::Message {
                    seq: 1,
                    opens_task: true,
                },
                mine: false,
                now: Instant::now(),
            },
        );

        assert!(
            !applied.news,
            "an opening that comes after the end of its task is not worth surfacing"
        );
        assert!(tasks.is_empty(), "and it opens no record");
    }

    /// A follow-up that was sent into a task that is over, or to another peer than its worker,
    /// takes no seq.
    #[test]
    fn a_follow_up_sent_to_a_finished_task_takes_no_seq() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        super::note_follow_up_sent(&mut tasks, &tid(), &peer_key("stranger"), (2, start));
        assert_eq!(tasks[&tid()].seq, 0, "another peer is not the worker");

        tasks
            .get_mut(&tid())
            .expect("the test inserted this task earlier")
            .close(TaskState::Completed, start);
        super::note_follow_up_sent(&mut tasks, &tid(), &peer_key("calm-otter"), (2, start));
        assert_eq!(tasks[&tid()].seq, 0, "a finished task takes no more seq");
    }

    /// A leg that arrives after the task is over changes nothing, shown progress and beats
    /// included; only the leg that closes the task moves its record.
    #[test]
    fn a_leg_after_the_end_changes_nothing() {
        let start = Instant::now();
        let mut tasks = working_task_we_opened(start);
        receive(
            &mut tasks,
            &status_frame(
                TaskState::Completed,
                Some(serde_json::json!({ "mesh:seq": 5 })),
            ),
        );
        let before = (
            tasks[&tid()].shown_progress,
            tasks[&tid()].peer_seq,
            tasks[&tid()].waiting_user,
            tasks[&tid()].peer_beat.is_some(),
        );

        let mut beat_metadata =
            status_update_params::beat_metadata_with_extras(status_update_params::BeatExtras {
                progress: Some(progress_of(30)),
                seq: Some(9),
                ..Default::default()
            });
        beat_metadata["mesh:seq"] = 9.into();
        receive(
            &mut tasks,
            &status_frame(TaskState::Working, Some(beat_metadata)),
        );
        receive(&mut tasks, &checkin_leg("late", "40", true));

        let after = (
            tasks[&tid()].shown_progress,
            tasks[&tid()].peer_seq,
            tasks[&tid()].waiting_user,
            tasks[&tid()].peer_beat.is_some(),
        );
        assert_eq!(
            after, before,
            "a finished task is frozen for every later leg"
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
    fn a_returned_task_is_not_reconciled_from_a_key_that_shares_the_worker_name() {
        let now = Instant::now();
        let mut tasks = working_task_we_opened(now);
        super::reconcile_initiator(
            &mut tasks,
            super::ReconcileParams {
                task_id: &tid(),
                peer_key: &peer_key("calm-otter-twin"),
                snapshot: snapshot_at(TaskState::Failed, 9),
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
                    news: true,
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
            news: true,
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
