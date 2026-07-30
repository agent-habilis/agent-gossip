use std::collections::VecDeque;
use std::time::Duration;

use tokio::time::Instant as TokioInstant;

use crate::output::OutputEvent;

/// One surfaced event tagged with its daemon-local sequence number. The `seq`
/// is the `poll` / `fetch` cursor value; `event` renders to the live-stream
/// JSON line via [`crate::output::surfaced_event_json`].
#[derive(Debug, Clone)]
pub struct SurfacedEvent {
    pub seq: u64,
    pub event: OutputEvent,
}

/// A bounded, seq-ordered record of everything the daemon **surfaced** to the
/// operator/agent — the history `poll` / `fetch_messages` drain.
///
/// Deliberately distinct from [`agent_habilis_mesh::runtime::message_log::MessageLog`]: that is
/// the cross-node anti-entropy buffer, whose retention is a deterministic
/// function of the message *set* (`eviction_key`) so every node agrees on what
/// survives. This buffer is **local**: a single monotonic `seq` records
/// *surfacing order* on this node, so one `--after <seq>` cursor walks chat,
/// presence, task legs, and the transient events (`ping_report`,
/// `peer_timeout`/`return`, `task_timeout`, `fork`) that never enter the message
/// log. Mixing the two would couple a local cursor to the cross-node eviction
/// order and break anti-entropy convergence — hence two buffers.
///
/// Oldest-drop on overflow: the front (lowest seq) is evicted first, so the
/// retained window is always the most-recently-surfaced `capacity` events.
pub(crate) struct SurfacedEvents {
    capacity: usize,
    next_seq: u64,
    events: VecDeque<SurfacedEvent>,
    /// The newest seq assigned to a *waking* event (see
    /// [`super::node::wakes`]). State/meta document echoes enter the ring but
    /// never bump this, so a parked bell is not rung by content nobody prints
    /// or reacts to — it rides along with the next waking batch instead.
    latest_waking_seq: u64,
}

impl SurfacedEvents {
    pub(crate) fn new(capacity: usize) -> Self {
        SurfacedEvents {
            capacity,
            // seq 0 is reserved as the "before anything" cursor: the first
            // pushed event is seq 1, so `since(Some(0))` returns the whole
            // buffer without a spurious eviction signal.
            next_seq: 1,
            events: VecDeque::with_capacity(capacity.min(64)),
            latest_waking_seq: 0,
        }
    }

    /// Append an event, assigning it the next seq, evicting the oldest if over
    /// capacity. Returns the assigned seq.
    pub(crate) fn push(&mut self, event: OutputEvent) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        if super::node::wakes(&event) {
            self.latest_waking_seq = seq;
        }
        self.events.push_back(SurfacedEvent { seq, event });
        while self.events.len() > self.capacity {
            self.events.pop_front();
        }
        seq
    }

    /// The newest assigned seq, or 0 when nothing has been surfaced yet
    /// (`next_seq` starts at 1). A long-poll waiter baselines on this: a
    /// `wait_from` of 0 on an empty ring fires only once a genuinely new event
    /// (seq 1) lands. Climbs monotonically and is unaffected by eviction.
    pub(crate) fn latest_seq(&self) -> u64 {
        self.next_seq - 1
    }

    /// The newest seq assigned to a waking event (0 when none yet). The bell's
    /// wake threshold — see [`SurfacedState::fulfill_ready_poll_waiters`].
    pub(crate) fn latest_waking_seq(&self) -> u64 {
        self.latest_waking_seq
    }

    /// The events surfaced after `after`, in seq order, plus `missed_before`.
    ///
    /// - `after == None` → the whole buffer (a first poll), `missed_before` none.
    /// - `after == Some(seq)` → every event with `seq > after`. `missed_before`
    ///   is `Some(oldest_retained_seq)` when `after` precedes the oldest
    ///   retained seq *and* is not 0 — i.e. the cursor aged out, so the caller
    ///   missed everything below that seq and must re-baseline. The `poll`
    ///   response leads with a `gap` line saying so; it is **not** reported by
    ///   emitting an `info`, which would feed back into this very ring (see
    ///   [`SurfacedState::poll_since`]).
    ///
    /// A cursor at or past the newest seq returns an empty slice and no gap
    /// (caller is simply up to date).
    pub(crate) fn since(&self, after: Option<u64>) -> (Vec<SurfacedEvent>, Option<u64>) {
        let Some(after) = after else {
            return (self.events.iter().cloned().collect(), None);
        };
        let missed_before = self
            .events
            .front()
            .filter(|oldest| after != 0 && after < oldest.seq.saturating_sub(1))
            .map(|oldest| oldest.seq);
        let slice = self
            .events
            .iter()
            .filter(|item| item.seq > after)
            .cloned()
            .collect();
        (slice, missed_before)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.events.len()
    }
}

/// One poll's answer: the events, plus whether the reader's cursor aged out of
/// the ring before them.
///
/// `missed_before` is the whole point of the type. Without it a reader whose
/// cursor fell off the back of the ring receives a normal-looking window and
/// silently skips everything below it — the loss is indistinguishable from
/// "nothing happened".
#[derive(Debug, Default)]
pub struct PollBatch {
    pub events: Vec<SurfacedEvent>,
    /// `Some(oldest_retained_seq)`: every event before this seq was evicted
    /// unread. The reader must re-baseline on the returned window.
    pub missed_before: Option<u64>,
}

/// The clean-shutdown answer to a parked long-poll — an IPC control response,
/// never printed and never an event. `"[]"` cannot serve here: it means "no
/// news, re-issue", so a bell receiving it at shutdown reconnects to a socket
/// that is about to vanish and dies with an error. The sentinel lets the CLI
/// exit 0 instead; a daemon that dies *without* sending it (crash, SIGKILL)
/// still produces a loud nonzero bell exit.
pub(crate) const SHUTDOWN_SENTINEL: &str = r#"{"shutdown":true}"#;

/// How a fulfilled/expired long-poll waiter's batch is delivered, per
/// transport: the CLI/IPC path wants the JSON-array string the `Poll` arm
/// builds today; the in-process path wants the typed events the caller renders.
/// Both carry the gap — a loss the JSON reader learns about must not be hidden
/// from the typed one.
pub(crate) enum PollResponder {
    Json(tokio::sync::oneshot::Sender<String>),
    Typed(tokio::sync::oneshot::Sender<PollBatch>),
}

impl PollResponder {
    /// Send the empty result for this transport (a timeout or a registry-full
    /// degrade — cases where "re-issue" is the right client move). Dropped
    /// receiver is fine — the client already went away.
    fn send_empty(self) {
        match self {
            PollResponder::Json(tx) => {
                let _ = tx.send("[]".to_string());
            }
            PollResponder::Typed(tx) => {
                let _ = tx.send(PollBatch::default());
            }
        }
    }

    /// Send the clean-shutdown result: the [`SHUTDOWN_SENTINEL`] on the
    /// CLI/socket transport (so a parked bell exits 0 instead of re-issuing
    /// into a vanishing socket); the plain empty batch on the in-process
    /// transport, whose channel-closure semantics were already clean.
    fn send_shutdown(self) {
        match self {
            PollResponder::Json(tx) => {
                let _ = tx.send(SHUTDOWN_SENTINEL.to_string());
            }
            PollResponder::Typed(tx) => {
                let _ = tx.send(PollBatch::default());
            }
        }
    }

    /// Send a fulfilled batch for this transport.
    fn send_batch(self, batch: PollBatch) {
        match self {
            PollResponder::Json(tx) => {
                let _ = tx.send(render_poll_array(&batch));
            }
            PollResponder::Typed(tx) => {
                let _ = tx.send(batch);
            }
        }
    }
}

/// What a parked long-poll waits to be exceeded before it fires.
#[derive(Clone, Copy)]
pub(crate) enum PollBaseline {
    /// Legacy explicit cursor: fire once `latest_seq() > seq`. Any push —
    /// waking or not — counts, preserving the pre-cursor `--after N --long`
    /// contract for old clients.
    Seq(u64),
    /// The bell: fire once `latest_waking_seq() > last_served`, both read
    /// LIVE at fulfillment time. The dynamic baseline is what closes the
    /// re-arm race in both directions: an unserved waking event fires a
    /// just-armed bell immediately, and a foreground poll advancing
    /// `last_served` under a parked bell keeps it parked instead of firing
    /// it spuriously.
    Served,
}

/// A blocked poll, waiting for its [`PollBaseline`] to be exceeded or for
/// `deadline` to elapse.
pub(crate) struct PollWaiter {
    baseline: PollBaseline,
    deadline: TokioInstant,
    responder: PollResponder,
}

/// Render a poll batch to the `poll` JSON-array string — the single source of
/// truth shared by the immediate `Poll` arm and a fulfilled long-poll, so the
/// two can never drift. Mirrors the per-event `filter_map` the IPC `Poll` arm
/// uses (`surfaced_event_json` returns `None` for an unrenderable event, which
/// is dropped, never `unwrap`ped).
///
/// A `gap` marker leads the array when the reader's cursor aged out. It is
/// synthesized *here*, at render time, and never pushed into the ring: the
/// ring is fed by the user-facing sink, so a real event announcing the loss
/// would itself become a ring entry (and, on the api path, a live
/// `events()` item). It carries no `seq` — it is a statement about the
/// events that are missing, not one of them — so a reader advancing `--after`
/// off the highest returned `seq` ignores it naturally.
pub(crate) fn render_poll_array(batch: &PollBatch) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(batch.events.len() + 1);
    if let Some(missed_before) = batch.missed_before {
        lines.push(format!(
            r#"{{"event":"gap","missed_before":{missed_before}}}"#
        ));
    }
    lines.extend(
        batch
            .events
            .iter()
            .filter_map(|item| crate::output::surfaced_event_json(item.seq, &item.event)),
    );
    format!("[{}]", lines.join(","))
}

/// The value cluster [`SurfacedState::poll_or_register`] needs beyond its
/// `&mut self` ring/registry handle.
pub(crate) struct PollOrRegisterParams {
    pub(crate) after: Option<u64>,
    pub(crate) long: bool,
    pub(crate) now: TokioInstant,
    pub(crate) responder: PollResponder,
}

/// The surfaced-events ring plus the parked long-poll waiters — the a2a layer's
/// slice of the daemon's surfacing state, held by [`A2aApp`](super::app::A2aApp)
/// so the daemon engine never names `OutputEvent`.
pub(crate) struct SurfacedState {
    /// Local, seq-ordered record of everything surfaced to the operator/agent —
    /// the history `poll` / `fetch_messages` drain. Fed by the `Output` tap, so
    /// it carries the *same* events the `--output json` stream shows, transient
    /// events included. Cursored by a monotonic local `seq` — deliberately
    /// separate from `message_log`'s cross-node `eviction_key`.
    surfaced_events: SurfacedEvents,
    /// Parked long-poll waiters: blocking `poll` / `fetch_messages` calls that
    /// found the buffer empty and are waiting for a new surfaced event or their
    /// deadline. Bounded by [`POLL_WAITERS_CAP`](crate::a2a::tuning::POLL_WAITERS_CAP);
    /// fulfilled right after a drain, expired by the loop's poll-deadline arm.
    poll_waiters: Vec<PollWaiter>,
    /// The highest seq ever handed to a *foreground* (non-long) poll — the
    /// daemon-side read cursor. A cursor-less `poll` serves everything beyond
    /// it and advances it; a cursor-less `poll --long` (the bell) parks until
    /// a waking event exceeds it. Long-poll deliveries never advance it: a
    /// bell's output is discarded by contract, so nothing was "served".
    /// In-memory only — a fresh daemon starts at 0 (nothing served).
    last_served: u64,
}

impl SurfacedState {
    pub(crate) fn new() -> Self {
        Self {
            surfaced_events: SurfacedEvents::new(crate::a2a::tuning::SURFACED_EVENTS_CAP),
            poll_waiters: Vec::new(),
            last_served: 0,
        }
    }

    /// Record a surfaced event in the ring (the `Output` tap drain).
    pub(crate) fn push(&mut self, event: OutputEvent) {
        self.surfaced_events.push(event);
    }

    /// The events surfaced after the `after` seq cursor, in surfacing order —
    /// the single source of truth for the CLI socket `poll` and the typed
    /// in-process `Poll` (api `fetch` / MCP `fetch_messages`). Reads the local
    /// [`surfaced_events`](Self::surfaced_events) ring, NOT the cross-node
    /// message log, so one `seq` cursor walks chat, presence, task legs, and the
    /// transient events alike. Join-horizon needs no re-filtering here: a
    /// pre-join message is never *surfaced*, so it never entered this ring.
    ///
    /// An aged-out cursor is reported to the caller in the returned
    /// [`PollBatch::missed_before`], **not** by emitting an `info`/`error`
    /// through the daemon's user-facing sink: that sink carries the
    /// surfaced-events tap, so a notice emitted here would feed straight back
    /// into the very ring being polled (and, on the api/Capture path, into
    /// the live `events()` subscription). Purely developer-facing diagnostics
    /// still go to `tracing`.
    pub(crate) fn poll_since(&self, after: Option<u64>) -> PollBatch {
        let (mut events, mut missed_before) = self.surfaced_events.since(after);
        // Cap the response to the fixed IPC window. The ring is sized to match
        // the window (see `SURFACED_EVENTS_CAP`), so in the steady state this is
        // a no-op; it only trims if a future ring grows past the window.
        if events.len() > crate::a2a::tuning::POLL_RESPONSE_MAX_MSGS {
            let drop_count = events.len() - crate::a2a::tuning::POLL_RESPONSE_MAX_MSGS;
            events.drain(0..drop_count);
            tracing::debug!(dropped = drop_count, "poll: response capped to the window");
            // Trimming drops events the caller had not seen — the same loss as
            // an aged-out cursor, and it must be reported the same way. The
            // surviving front is the new floor.
            missed_before = events.first().map(|item| item.seq).or(missed_before);
        }
        tracing::debug!(returned = events.len(), missed_before, "poll served");
        PollBatch {
            events,
            missed_before,
        }
    }

    /// Register a blocking poll with its wait [`PollBaseline`].
    ///
    /// Returns `None` once the waiter is parked. If the registry is at
    /// `POLL_WAITERS_CAP`, the waiter is **not** parked and the responder is
    /// handed back so the caller degrades to an immediate (empty) response —
    /// the registry can never grow without bound.
    pub(crate) fn register_poll_waiter(
        &mut self,
        baseline: PollBaseline,
        deadline: TokioInstant,
        responder: PollResponder,
    ) -> Option<PollResponder> {
        if self.poll_waiters.len() >= crate::a2a::tuning::POLL_WAITERS_CAP {
            return Some(responder);
        }
        self.poll_waiters.push(PollWaiter {
            baseline,
            deadline,
            responder,
        });
        None
    }

    /// Advance the read cursor past everything a foreground poll just served.
    /// `max` keeps a replay (hidden `--after 0`) from regressing it.
    fn advance_last_served(&mut self, batch: &PollBatch) {
        if let Some(newest) = batch.events.last().map(|item| item.seq) {
            self.last_served = self.last_served.max(newest);
        }
    }

    /// Serve a `poll` / `fetch_messages` read, blocking if asked. The single
    /// policy shared by the CLI/IPC and in-process arms so they can't drift.
    ///
    /// Cursor-less (`after: None`) calls use the daemon-side read cursor:
    /// - not `long`: serve everything beyond `last_served` and advance it —
    ///   the foreground content read;
    /// - `long` (the bell): respond immediately when an unserved *waking*
    ///   event (or a gap) exists, else park a [`PollBaseline::Served`] waiter.
    ///   Never advances the cursor — a bell's output is discarded.
    ///
    /// Explicit-`after` calls keep the legacy stateless contract:
    /// - not `long`: serve past `after`; also advances `last_served` (via
    ///   `max`) so an old-protocol client's foreground reads keep a new
    ///   daemon's bell from re-firing forever;
    /// - `long`: immediate on any event past `after`, else park a
    ///   [`PollBaseline::Seq`] waiter.
    ///
    /// `now` is the registration instant (passed in so tests can pin it).
    pub(crate) fn poll_or_register(&mut self, params: PollOrRegisterParams) {
        let PollOrRegisterParams {
            after,
            long,
            now,
            responder,
        } = params;
        let (batch, baseline) = match after {
            None => (
                self.poll_since(Some(self.last_served)),
                PollBaseline::Served,
            ),
            Some(after) => (self.poll_since(Some(after)), PollBaseline::Seq(after)),
        };
        if !long {
            self.advance_last_served(&batch);
            responder.send_batch(batch);
            return;
        }
        // A gap is news even for a bell: the caller must learn it lost events
        // rather than park as if it were up to date.
        let fire_now = batch.missed_before.is_some()
            || match baseline {
                PollBaseline::Served => self.surfaced_events.latest_waking_seq() > self.last_served,
                PollBaseline::Seq(_) => !batch.events.is_empty(),
            };
        if fire_now {
            responder.send_batch(batch);
            return;
        }
        let deadline = now + Duration::from_millis(crate::a2a::tuning::longpoll_max_ms());
        if let Some(unregistered) = self.register_poll_waiter(baseline, deadline, responder) {
            unregistered.send_empty(); // registry full → degrade to immediate
        }
    }

    /// Deliver to every parked waiter whose baseline has been exceeded —
    /// `latest_seq()` for a legacy [`PollBaseline::Seq`], the live
    /// `latest_waking_seq() > last_served` condition for the bell. Called
    /// right after a drain each loop iteration; the batch is computed via
    /// [`poll_since`](Self::poll_since) so the response cap + logging match
    /// an immediate poll exactly.
    pub(crate) fn fulfill_ready_poll_waiters(&mut self) {
        let latest = self.surfaced_events.latest_seq();
        let latest_waking = self.surfaced_events.latest_waking_seq();
        let last_served = self.last_served;
        let is_ready = move |waiter: &PollWaiter| match waiter.baseline {
            PollBaseline::Seq(seq) => seq < latest,
            PollBaseline::Served => last_served < latest_waking,
        };
        if !self.poll_waiters.iter().any(is_ready) {
            return; // nothing advanced; cheap fast path
        }
        // Split ready vs. still-waiting without borrowing `self` across the
        // `poll_since` call (which needs `&self`).
        let mut still_waiting = Vec::with_capacity(self.poll_waiters.len());
        let ready: Vec<PollWaiter> = std::mem::take(&mut self.poll_waiters)
            .into_iter()
            .filter_map(|waiter| {
                if is_ready(&waiter) {
                    Some(waiter)
                } else {
                    still_waiting.push(waiter);
                    None
                }
            })
            .collect();
        for waiter in ready {
            let base = match waiter.baseline {
                PollBaseline::Seq(seq) => seq,
                PollBaseline::Served => self.last_served,
            };
            let batch = self.poll_since(Some(base));
            waiter.responder.send_batch(batch);
        }
        self.poll_waiters = still_waiting;
    }

    /// The earliest waiter deadline, for the loop's `sleep_until_opt` arm.
    /// `None` (no waiters) makes that arm pend forever.
    pub(crate) fn earliest_poll_deadline(&self) -> Option<TokioInstant> {
        self.poll_waiters.iter().map(|waiter| waiter.deadline).min()
    }

    /// Send the empty (timeout) result to every waiter whose deadline has
    /// passed and drop it.
    pub(crate) fn expire_poll_waiters(&mut self, now: TokioInstant) {
        let survivors: Vec<PollWaiter> = std::mem::take(&mut self.poll_waiters)
            .into_iter()
            .filter_map(|waiter| {
                if waiter.deadline <= now {
                    waiter.responder.send_empty();
                    None
                } else {
                    Some(waiter)
                }
            })
            .collect();
        self.poll_waiters = survivors;
    }

    /// Drain every parked waiter with the clean-shutdown result — called on
    /// event-loop shutdown so a held long-poll ends cleanly (exit 0) instead
    /// of re-issuing into a socket that is about to vanish.
    pub(crate) fn close_poll_waiters(&mut self) {
        for waiter in std::mem::take(&mut self.poll_waiters) {
            waiter.responder.send_shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PollBaseline, PollOrRegisterParams, PollResponder, SurfacedEvents, SurfacedState,
        render_poll_array,
    };
    use crate::output::OutputEvent;
    use agent_habilis_mesh::protocol::{Channel, Message, MessageKind, Nickname};
    use std::time::Duration;
    use tokio::time::Instant as TokioInstant;

    fn nick(name: &str) -> Nickname {
        Nickname::new(name.to_owned()).expect("valid test nickname")
    }

    fn peer_return(name: &str) -> OutputEvent {
        OutputEvent::PeerReturn {
            nickname: nick(name),
        }
    }

    /// A meta/state document echo — pollable but non-waking.
    fn meta_changed() -> OutputEvent {
        OutputEvent::StateChanged {
            channel: Channel::Meta,
            event: Box::new(Message::fixture(MessageKind::Meta, "{}")),
            document: serde_json::json!({}),
            is_self: true,
        }
    }

    #[test]
    fn first_poll_returns_all_no_eviction() {
        let mut buf = SurfacedEvents::new(10);
        buf.push(peer_return("a"));
        buf.push(peer_return("b"));
        let (events, missed_before) = buf.since(None);
        assert_eq!(events.len(), 2);
        assert!(missed_before.is_none());
        assert_eq!(events[0].seq, 1);
        assert_eq!(events[1].seq, 2);
    }

    #[test]
    fn cursor_returns_only_newer() {
        let mut buf = SurfacedEvents::new(10);
        let s1 = buf.push(peer_return("a"));
        buf.push(peer_return("b"));
        buf.push(peer_return("c"));
        let (events, missed_before) = buf.since(Some(s1));
        assert!(missed_before.is_none());
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 2);
        assert_eq!(events[1].seq, 3);
    }

    #[test]
    fn cursor_zero_returns_all_without_eviction() {
        let mut buf = SurfacedEvents::new(2);
        // Overflow so the front seq is > 1, proving seq 0 is never "evicted".
        for name in ["a", "b", "c", "d"] {
            buf.push(peer_return(name));
        }
        let (events, missed_before) = buf.since(Some(0));
        assert!(
            missed_before.is_none(),
            "the 0 cursor is the before-anything baseline"
        );
        assert_eq!(events.len(), 2, "only the retained window");
    }

    #[test]
    fn cursor_at_newest_returns_empty() {
        let mut buf = SurfacedEvents::new(10);
        buf.push(peer_return("a"));
        let s2 = buf.push(peer_return("b"));
        let (events, missed_before) = buf.since(Some(s2));
        assert!(events.is_empty());
        assert!(missed_before.is_none());
    }

    #[test]
    fn cursor_at_evicted_seq_is_not_a_gap() {
        // Evicting the event *at* the cursor loses nothing: the caller already
        // consumed it, and the next expected seq is still present.
        let mut buf = SurfacedEvents::new(2);
        let consumed = buf.push(peer_return("a")); // seq 1, evicted next
        buf.push(peer_return("b")); // seq 2
        buf.push(peer_return("c")); // seq 3 → evicts seq 1; ring {2,3}
        let (events, missed_before) = buf.since(Some(consumed));
        assert!(
            missed_before.is_none(),
            "cursor at the evicted seq still has seq 2 next — no gap"
        );
        assert_eq!(events.len(), 2, "returns seq 2 and 3");
    }

    #[test]
    fn evicted_cursor_flags_gap() {
        // A real gap: the cursor's next-expected seq was evicted unseen.
        let mut buf = SurfacedEvents::new(2);
        let stale = buf.push(peer_return("a")); // seq 1 (the cursor)
        buf.push(peer_return("b")); // seq 2 → evicted unseen
        buf.push(peer_return("c")); // seq 3
        buf.push(peer_return("d")); // seq 4 → evicts seq 2; ring {3,4}
        assert_eq!(buf.len(), 2);
        let (events, missed_before) = buf.since(Some(stale));
        assert_eq!(
            missed_before,
            Some(3),
            "seq 2 aged out unseen; the oldest surviving seq is the new floor"
        );
        // Still returns the current window so the caller can re-baseline.
        assert_eq!(events.len(), 2);
    }

    /// The gap must survive rendering. `since` has always computed it; the
    /// regression this guards is `poll_since` dropping it on the floor, which
    /// left a reader that had lost events unable to tell.
    #[test]
    fn evicted_cursor_renders_a_leading_gap_marker() {
        let mut state = SurfacedState::new();
        // Overflow a small ring by hand: push past the cursor's next-expected
        // seq so it ages out unseen.
        for name in ["a", "b", "c"] {
            state.push(peer_return(name));
        }
        // Force the eviction the ring cap would cause in production.
        while state.surfaced_events.len() > 1 {
            state.surfaced_events.events.pop_front();
        }
        let batch = state.poll_since(Some(1));
        assert_eq!(batch.missed_before, Some(3), "seq 2 was lost");

        let rendered = render_poll_array(&batch);
        assert!(
            rendered.starts_with(r#"[{"event":"gap","missed_before":3}"#),
            "the gap must lead the array, before any event: {rendered}"
        );
        assert!(
            !rendered.contains(r#""event":"gap","missed_before":3},{"event":"gap""#),
            "exactly one gap marker"
        );
    }

    #[test]
    fn clean_poll_renders_no_gap_marker() {
        let mut state = SurfacedState::new();
        state.push(peer_return("a"));
        let batch = state.poll_since(Some(0));
        assert!(batch.missed_before.is_none());
        assert!(
            !render_poll_array(&batch).contains(r#""event":"gap""#),
            "a reader that lost nothing is told nothing"
        );
    }

    #[test]
    fn latest_seq_starts_at_zero_climbs_and_survives_eviction() {
        let mut buf = SurfacedEvents::new(2);
        assert_eq!(buf.latest_seq(), 0, "fresh ring has no surfaced events");
        buf.push(peer_return("a"));
        assert_eq!(buf.latest_seq(), 1);
        buf.push(peer_return("b"));
        buf.push(peer_return("c")); // evicts seq 1; ring {2,3}
        assert_eq!(
            buf.latest_seq(),
            3,
            "latest_seq tracks the newest assigned seq, not the retained front"
        );
    }

    #[test]
    fn oldest_drop_keeps_recent_window() {
        let mut buf = SurfacedEvents::new(3);
        for name in ["a", "b", "c", "d", "e"] {
            buf.push(peer_return(name));
        }
        assert_eq!(buf.len(), 3);
        let (events, _) = buf.since(None);
        let seqs: Vec<u64> = events.iter().map(|item| item.seq).collect();
        assert_eq!(seqs, vec![3, 4, 5], "kept the three newest by seq");
    }

    #[tokio::test]
    async fn waiter_fulfilled_by_a_new_event() {
        let mut surfaced = SurfacedState::new();
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        // Empty ring → register a bell (nothing served yet).
        assert!(
            surfaced
                .register_poll_waiter(
                    PollBaseline::Served,
                    TokioInstant::now() + Duration::from_secs(30),
                    PollResponder::Json(tx)
                )
                .is_none(),
            "registered, not degraded"
        );
        // A new waking event lands and the loop drains+fulfills.
        surfaced.push(peer_return("a"));
        surfaced.fulfill_ready_poll_waiters();
        let body = rx.await.expect("waiter was fulfilled");
        assert!(
            body.starts_with('[') && body.len() > 2,
            "non-empty batch: {body}"
        );
        assert!(surfaced.poll_waiters.is_empty(), "fulfilled waiter removed");
    }

    #[tokio::test]
    async fn waiter_expires_empty_at_deadline() {
        let mut surfaced = SurfacedState::new();
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        let past = TokioInstant::now();
        surfaced.register_poll_waiter(PollBaseline::Served, past, PollResponder::Json(tx));
        // No event; the deadline (now) has passed.
        surfaced.expire_poll_waiters(TokioInstant::now() + Duration::from_millis(1));
        assert_eq!(rx.await.expect("waiter expired"), "[]", "timeout → empty");
        assert!(surfaced.poll_waiters.is_empty());
    }

    #[tokio::test]
    async fn registry_at_cap_degrades_to_immediate() {
        let mut surfaced = SurfacedState::new();
        let deadline = TokioInstant::now() + Duration::from_secs(30);
        // Fill the registry to the cap with throwaway waiters.
        for _ in 0..crate::a2a::tuning::POLL_WAITERS_CAP {
            let (tx, _rx) = tokio::sync::oneshot::channel::<String>();
            assert!(
                surfaced
                    .register_poll_waiter(PollBaseline::Served, deadline, PollResponder::Json(tx))
                    .is_none()
            );
        }
        // One more must be handed back (degrade), not parked.
        let (tx, _rx) = tokio::sync::oneshot::channel::<String>();
        assert!(
            surfaced
                .register_poll_waiter(PollBaseline::Served, deadline, PollResponder::Json(tx))
                .is_some(),
            "over the cap → responder returned to caller"
        );
        assert_eq!(
            surfaced.poll_waiters.len(),
            crate::a2a::tuning::POLL_WAITERS_CAP
        );
    }

    /// Plain (cursor) polls advance `last_served`; the next one returns only
    /// what landed since — no client-side cursor involved.
    #[tokio::test]
    async fn cursor_poll_serves_unserved_and_advances() {
        let mut surfaced = SurfacedState::new();
        surfaced.push(peer_return("a"));
        surfaced.push(peer_return("b"));

        let (first_tx, first_rx) = tokio::sync::oneshot::channel::<String>();
        surfaced.poll_or_register(PollOrRegisterParams {
            after: None,
            long: false,
            now: TokioInstant::now(),
            responder: PollResponder::Json(first_tx),
        });
        let first = first_rx.await.expect("immediate");
        assert!(first.contains("\"seq\":1") && first.contains("\"seq\":2"));
        assert_eq!(surfaced.last_served, 2, "cursor advanced past the batch");

        // Nothing new → empty, cursor unchanged.
        let (empty_tx, empty_rx) = tokio::sync::oneshot::channel::<String>();
        surfaced.poll_or_register(PollOrRegisterParams {
            after: None,
            long: false,
            now: TokioInstant::now(),
            responder: PollResponder::Json(empty_tx),
        });
        assert_eq!(empty_rx.await.expect("immediate"), "[]");
        assert_eq!(surfaced.last_served, 2);

        // One more event → only it is served.
        surfaced.push(peer_return("c"));
        let (third_tx, third_rx) = tokio::sync::oneshot::channel::<String>();
        surfaced.poll_or_register(PollOrRegisterParams {
            after: None,
            long: false,
            now: TokioInstant::now(),
            responder: PollResponder::Json(third_tx),
        });
        let third = third_rx.await.expect("immediate");
        assert!(third.contains("\"seq\":3") && !third.contains("\"seq\":2"));
        assert_eq!(surfaced.last_served, 3);
    }

    /// An explicit-`after` replay (hidden debug flag) advances via `max`, so
    /// `--after 0` cannot regress the cursor, while an old-protocol client's
    /// foreground reads still move it forward.
    #[tokio::test]
    async fn explicit_after_advances_by_max_never_regresses() {
        let mut surfaced = SurfacedState::new();
        surfaced.push(peer_return("a"));
        surfaced.push(peer_return("b"));
        let (read_tx, read_rx) = tokio::sync::oneshot::channel::<String>();
        surfaced.poll_or_register(PollOrRegisterParams {
            after: Some(1),
            long: false,
            now: TokioInstant::now(),
            responder: PollResponder::Json(read_tx),
        });
        read_rx.await.expect("immediate");
        assert_eq!(surfaced.last_served, 2, "explicit read advanced");

        let (replay_tx, replay_rx) = tokio::sync::oneshot::channel::<String>();
        surfaced.poll_or_register(PollOrRegisterParams {
            after: Some(0),
            long: false,
            now: TokioInstant::now(),
            responder: PollResponder::Json(replay_tx),
        });
        replay_rx.await.expect("immediate");
        assert_eq!(surfaced.last_served, 2, "replay did not regress");
    }

    /// The bell parks over unserved NON-waking events (a self meta echo must
    /// not ring it), fires on the next waking push, and its batch carries the
    /// non-waking events along.
    #[tokio::test]
    async fn bell_ignores_meta_but_delivers_it_with_the_next_waking_batch() {
        let mut surfaced = SurfacedState::new();
        surfaced.push(meta_changed()); // seq 1, non-waking

        let (tx, mut rx) = tokio::sync::oneshot::channel::<String>();
        surfaced.poll_or_register(PollOrRegisterParams {
            after: None,
            long: true,
            now: TokioInstant::now(),
            responder: PollResponder::Json(tx),
        });
        assert_eq!(surfaced.poll_waiters.len(), 1, "parked over the meta echo");
        surfaced.fulfill_ready_poll_waiters();
        assert!(rx.try_recv().is_err(), "meta alone never rings the bell");

        surfaced.push(peer_return("a")); // seq 2, waking
        surfaced.fulfill_ready_poll_waiters();
        let body = rx.await.expect("woken by the waking event");
        assert!(
            body.contains("\"seq\":1") && body.contains("\"seq\":2"),
            "the meta echo rides along: {body}"
        );
        assert_eq!(surfaced.last_served, 0, "a bell delivery serves nothing");
    }

    /// The gap-window race: a waking event that landed after the foreground
    /// poll answered fires a just-armed bell immediately.
    #[tokio::test]
    async fn bell_armed_into_unserved_backlog_fires_immediately() {
        let mut surfaced = SurfacedState::new();
        surfaced.push(peer_return("a"));
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        surfaced.poll_or_register(PollOrRegisterParams {
            after: None,
            long: true,
            now: TokioInstant::now(),
            responder: PollResponder::Json(tx),
        });
        assert!(surfaced.poll_waiters.is_empty(), "never parked");
        let body = rx.await.expect("immediate fire");
        assert!(body.contains("\"seq\":1"));
    }

    /// The no-spurious-wake race: a foreground poll consuming the backlog
    /// while a bell is parked keeps the bell parked (the live baseline reads
    /// the advanced cursor).
    #[tokio::test]
    async fn bell_is_not_woken_by_a_foreground_poll_serving_the_backlog() {
        let mut surfaced = SurfacedState::new();
        // Bell first (empty ring → parks).
        let (bell_tx, mut bell_rx) = tokio::sync::oneshot::channel::<String>();
        surfaced.poll_or_register(PollOrRegisterParams {
            after: None,
            long: true,
            now: TokioInstant::now(),
            responder: PollResponder::Json(bell_tx),
        });
        // An event lands; the foreground poll (same message, other call)
        // serves it and advances the cursor BEFORE the fulfill pass runs.
        surfaced.push(peer_return("a"));
        let (fg_tx, fg_rx) = tokio::sync::oneshot::channel::<String>();
        surfaced.poll_or_register(PollOrRegisterParams {
            after: None,
            long: false,
            now: TokioInstant::now(),
            responder: PollResponder::Json(fg_tx),
        });
        assert!(fg_rx.await.expect("served").contains("\"seq\":1"));
        surfaced.fulfill_ready_poll_waiters();
        assert!(
            bell_rx.try_recv().is_err(),
            "everything is served — the bell must stay parked"
        );
        assert_eq!(surfaced.poll_waiters.len(), 1);
    }

    /// The shutdown drain is distinguishable from a timeout: the CLI/socket
    /// transport gets the sentinel (so a parked bell can exit 0 instead of
    /// re-issuing into a vanishing socket); the in-process transport keeps
    /// the plain empty batch.
    #[tokio::test]
    async fn shutdown_drain_sends_the_sentinel_to_json_waiters_only() {
        let mut surfaced = SurfacedState::new();
        let deadline = TokioInstant::now() + Duration::from_secs(30);
        let (json_tx, json_rx) = tokio::sync::oneshot::channel::<String>();
        let (typed_tx, typed_rx) = tokio::sync::oneshot::channel::<super::PollBatch>();
        surfaced.register_poll_waiter(PollBaseline::Served, deadline, PollResponder::Json(json_tx));
        surfaced.register_poll_waiter(
            PollBaseline::Served,
            deadline,
            PollResponder::Typed(typed_tx),
        );
        surfaced.close_poll_waiters();
        assert_eq!(
            json_rx.await.expect("json waiter answered"),
            super::SHUTDOWN_SENTINEL
        );
        let batch = typed_rx.await.expect("typed waiter answered");
        assert!(batch.events.is_empty() && batch.missed_before.is_none());
        assert!(surfaced.poll_waiters.is_empty());
    }

    /// Meta/state pushes bump `latest_seq` but never `latest_waking_seq`.
    #[test]
    fn waking_seq_ignores_document_echoes() {
        let mut buf = SurfacedEvents::new(10);
        buf.push(meta_changed());
        assert_eq!(buf.latest_seq(), 1);
        assert_eq!(buf.latest_waking_seq(), 0);
        buf.push(peer_return("a"));
        assert_eq!(buf.latest_waking_seq(), 2);
    }

    #[tokio::test]
    async fn poll_or_register_responds_immediately_when_buffered() {
        let mut surfaced = SurfacedState::new();
        surfaced.push(peer_return("a"));
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        // Buffer has events → respond now even though long is set; no waiter.
        surfaced.poll_or_register(PollOrRegisterParams {
            after: None,
            long: true,
            now: TokioInstant::now(),
            responder: PollResponder::Json(tx),
        });
        let body = rx.await.expect("immediate response");
        assert!(body.len() > 2, "non-empty: {body}");
        assert!(surfaced.poll_waiters.is_empty(), "never parked");
    }

    #[tokio::test]
    async fn poll_or_register_immediate_empty_when_not_long() {
        let mut surfaced = SurfacedState::new();
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        // Empty buffer, not long → immediate empty, no waiter.
        surfaced.poll_or_register(PollOrRegisterParams {
            after: None,
            long: false,
            now: TokioInstant::now(),
            responder: PollResponder::Json(tx),
        });
        assert_eq!(rx.await.expect("immediate"), "[]");
        assert!(surfaced.poll_waiters.is_empty());
    }

    #[tokio::test]
    async fn poll_or_register_long_parks_then_expires_at_cap() {
        let mut surfaced = SurfacedState::new();
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        let now = TokioInstant::now();
        // Empty buffer, long → parked with deadline now + longpoll_max_ms().
        surfaced.poll_or_register(PollOrRegisterParams {
            after: None,
            long: true,
            now,
            responder: PollResponder::Json(tx),
        });
        assert_eq!(surfaced.poll_waiters.len(), 1, "parked");
        let cap = Duration::from_millis(crate::a2a::tuning::longpoll_max_ms());
        surfaced.expire_poll_waiters(now + cap - Duration::from_millis(1));
        assert_eq!(
            surfaced.poll_waiters.len(),
            1,
            "still parked before the cap"
        );
        surfaced.expire_poll_waiters(now + cap + Duration::from_millis(1));
        assert_eq!(rx.await.expect("expired"), "[]", "cap elapsed → empty");
        assert!(surfaced.poll_waiters.is_empty());
    }
}
