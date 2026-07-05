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
/// Deliberately distinct from [`agent_habilis_gossip::daemon::message_log::MessageLog`]: that is
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
        }
    }

    /// Append an event, assigning it the next seq, evicting the oldest if over
    /// capacity. Returns the assigned seq.
    pub(crate) fn push(&mut self, event: OutputEvent) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
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

    /// The events surfaced after `after`, in seq order, plus an `evicted` flag.
    ///
    /// - `after == None` → the whole buffer (a first poll), `evicted == false`.
    /// - `after == Some(seq)` → every event with `seq > after`. `evicted` is
    ///   true when `after` precedes the oldest retained seq *and* is not 0 —
    ///   i.e. the cursor aged out, so the caller silently missed the gap and
    ///   should re-baseline (the `poll` handler emits an `info` notice, the
    ///   same contract as the message log's evicted-cursor path).
    ///
    /// A cursor at or past the newest seq returns an empty slice with
    /// `evicted == false` (caller is simply up to date).
    pub(crate) fn since(&self, after: Option<u64>) -> (Vec<SurfacedEvent>, bool) {
        let Some(after) = after else {
            return (self.events.iter().cloned().collect(), false);
        };
        let evicted = after != 0
            && self
                .events
                .front()
                .is_some_and(|oldest| after < oldest.seq.saturating_sub(1));
        let slice = self
            .events
            .iter()
            .filter(|item| item.seq > after)
            .cloned()
            .collect();
        (slice, evicted)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.events.len()
    }
}

/// How a fulfilled/expired long-poll waiter's batch is delivered, per
/// transport: the CLI/IPC path wants the JSON-array string the `Poll` arm
/// builds today; the in-process path wants the typed events the caller renders.
pub(crate) enum PollResponder {
    Json(tokio::sync::oneshot::Sender<String>),
    Typed(tokio::sync::oneshot::Sender<Vec<SurfacedEvent>>),
}

impl PollResponder {
    /// Send the empty result for this transport (a timeout, a registry-full
    /// degrade, or shutdown). Dropped receiver is fine — the client already
    /// went away.
    fn send_empty(self) {
        match self {
            PollResponder::Json(tx) => {
                let _ = tx.send("[]".to_string());
            }
            PollResponder::Typed(tx) => {
                let _ = tx.send(Vec::new());
            }
        }
    }

    /// Send a fulfilled batch for this transport.
    fn send_batch(self, events: Vec<SurfacedEvent>) {
        match self {
            PollResponder::Json(tx) => {
                let _ = tx.send(render_poll_array(&events));
            }
            PollResponder::Typed(tx) => {
                let _ = tx.send(events);
            }
        }
    }
}

/// A blocked poll, waiting for `surfaced_events` to advance past `wait_from`
/// or for `deadline` to elapse.
pub(crate) struct PollWaiter {
    /// Respond once `surfaced_events.latest_seq() > wait_from`. Captured from
    /// the live ring at registration so any later push is caught.
    wait_from: u64,
    deadline: TokioInstant,
    responder: PollResponder,
}

/// Render surfaced events to the `poll` JSON-array string — the single source
/// of truth shared by the immediate `Poll` arm and a fulfilled long-poll, so
/// the two can never drift. Mirrors the per-event `filter_map` the IPC `Poll`
/// arm uses (`surfaced_event_json` returns `None` for an unrenderable event,
/// which is dropped, never `unwrap`ped).
pub(crate) fn render_poll_array(events: &[SurfacedEvent]) -> String {
    let lines: Vec<String> = events
        .iter()
        .filter_map(|item| crate::output::surfaced_event_json(item.seq, &item.event))
        .collect();
    format!("[{}]", lines.join(","))
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
    /// deadline. Bounded by [`POLL_WAITERS_CAP`](agent_habilis_gossip::util::consts::POLL_WAITERS_CAP);
    /// fulfilled right after a drain, expired by the loop's poll-deadline arm.
    poll_waiters: Vec<PollWaiter>,
}

impl SurfacedState {
    pub(crate) fn new() -> Self {
        Self {
            surfaced_events: SurfacedEvents::new(
                agent_habilis_gossip::util::consts::SURFACED_EVENTS_CAP,
            ),
            poll_waiters: Vec::new(),
        }
    }

    /// Record a surfaced event in the ring (the `Output` tap drain).
    pub(crate) fn push(&mut self, event: OutputEvent) {
        self.surfaced_events.push(event);
    }

    /// The events surfaced after the `after` seq cursor, in surfacing order —
    /// the single source of truth for the CLI socket `poll` and the typed
    /// in-process `Poll` (embed `fetch` / MCP `fetch_messages`). Reads the local
    /// [`surfaced_events`](Self::surfaced_events) ring, NOT the cross-node
    /// message log, so one `seq` cursor walks chat, presence, task legs, and the
    /// transient events alike. Join-horizon needs no re-filtering here: a
    /// pre-join message is never *surfaced*, so it never entered this ring.
    ///
    /// Diagnostics (cursor aged out, response capped) go to the developer log
    /// via `tracing`, **not** through the daemon's user-facing sink: that sink
    /// carries the surfaced-events tap, so an `info`/`error` notice emitted here
    /// would feed straight back into the very ring being polled (and, on the
    /// embed/Capture path, into the live `events()` subscription).
    pub(crate) fn poll_since(&self, after: Option<u64>) -> Vec<SurfacedEvent> {
        let (mut events, evicted) = self.surfaced_events.since(after);
        if evicted {
            tracing::debug!(
                "poll: --after seq aged out of the ring; returning all surfaced events"
            );
        }
        // Cap the response to the fixed IPC window. The ring is sized to match
        // the window (see `SURFACED_EVENTS_CAP`), so in the steady state this is
        // a no-op; it only trims if a future ring grows past the window.
        if events.len() > agent_habilis_gossip::util::consts::POLL_RESPONSE_MAX_MSGS {
            let drop_count =
                events.len() - agent_habilis_gossip::util::consts::POLL_RESPONSE_MAX_MSGS;
            events.drain(0..drop_count);
            tracing::debug!(dropped = drop_count, "poll: response capped to the window");
        }
        tracing::debug!(returned = events.len(), evicted, "poll served");
        events
    }

    /// Register a blocking poll that found the buffer empty. The wait baseline
    /// is `after`, or the ring's current `latest_seq()` for a cursor-less call,
    /// captured here (synchronously, from the live ring) so any push after this
    /// point carries `seq > wait_from` and is caught by the next `fulfill`.
    ///
    /// Returns `None` once the waiter is parked. If the registry is at
    /// `POLL_WAITERS_CAP`, the waiter is **not** parked and the responder is
    /// handed back so the caller degrades to an immediate (empty) response —
    /// the registry can never grow without bound.
    pub(crate) fn register_poll_waiter(
        &mut self,
        after: Option<u64>,
        deadline: TokioInstant,
        responder: PollResponder,
    ) -> Option<PollResponder> {
        if self.poll_waiters.len() >= agent_habilis_gossip::util::consts::POLL_WAITERS_CAP {
            return Some(responder);
        }
        let wait_from = after.unwrap_or_else(|| self.surfaced_events.latest_seq());
        self.poll_waiters.push(PollWaiter {
            wait_from,
            deadline,
            responder,
        });
        None
    }

    /// Serve a `poll` / `fetch_messages` read, blocking if asked. The single
    /// policy shared by the CLI/IPC and in-process arms so they can't drift:
    ///
    /// 1. if the buffer already has events past `after`, respond immediately
    ///    (never make a caller with pending events wait);
    /// 2. else if not `long`, respond immediately with empty;
    /// 3. else register a waiter with deadline `now + longpoll_max_ms()` —
    ///    and if the registry is full, respond empty.
    ///
    /// `now` is the registration instant (passed in so tests can pin it).
    pub(crate) fn poll_or_register(
        &mut self,
        after: Option<u64>,
        long: bool,
        now: TokioInstant,
        responder: PollResponder,
    ) {
        let events = self.poll_since(after);
        if !events.is_empty() {
            responder.send_batch(events);
            return;
        }
        if !long {
            responder.send_empty();
            return;
        }
        let deadline =
            now + Duration::from_millis(agent_habilis_gossip::util::tuning::longpoll_max_ms());
        if let Some(unregistered) = self.register_poll_waiter(after, deadline, responder) {
            unregistered.send_empty(); // registry full → degrade to immediate
        }
    }

    /// Deliver to every parked waiter whose baseline the ring has advanced past
    /// (`latest_seq() > wait_from`). Called right after a drain each loop
    /// iteration; the batch is computed via [`poll_since`](Self::poll_since) so
    /// the response cap + logging match an immediate poll exactly.
    pub(crate) fn fulfill_ready_poll_waiters(&mut self) {
        let latest = self.surfaced_events.latest_seq();
        if self
            .poll_waiters
            .iter()
            .all(|waiter| waiter.wait_from >= latest)
        {
            return; // nothing advanced; cheap fast path
        }
        // Split ready vs. still-waiting without borrowing `self` across the
        // `poll_since` call (which needs `&self`).
        let mut still_waiting = Vec::with_capacity(self.poll_waiters.len());
        let ready: Vec<PollWaiter> = std::mem::take(&mut self.poll_waiters)
            .into_iter()
            .filter_map(|waiter| {
                if waiter.wait_from < latest {
                    Some(waiter)
                } else {
                    still_waiting.push(waiter);
                    None
                }
            })
            .collect();
        for waiter in ready {
            let batch = self.poll_since(Some(waiter.wait_from));
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

    /// Drain every parked waiter with an empty result — called on event-loop
    /// shutdown so a held long-poll returns a clean timeout-empty rather than a
    /// dropped-channel error.
    pub(crate) fn close_poll_waiters(&mut self) {
        for waiter in std::mem::take(&mut self.poll_waiters) {
            waiter.responder.send_empty();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PollResponder, SurfacedEvents, SurfacedState};
    use crate::output::OutputEvent;
    use agent_habilis_gossip::protocol::Nickname;
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

    #[test]
    fn first_poll_returns_all_no_eviction() {
        let mut buf = SurfacedEvents::new(10);
        buf.push(peer_return("a"));
        buf.push(peer_return("b"));
        let (events, evicted) = buf.since(None);
        assert_eq!(events.len(), 2);
        assert!(!evicted);
        assert_eq!(events[0].seq, 1);
        assert_eq!(events[1].seq, 2);
    }

    #[test]
    fn cursor_returns_only_newer() {
        let mut buf = SurfacedEvents::new(10);
        let s1 = buf.push(peer_return("a"));
        buf.push(peer_return("b"));
        buf.push(peer_return("c"));
        let (events, evicted) = buf.since(Some(s1));
        assert!(!evicted);
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
        let (events, evicted) = buf.since(Some(0));
        assert!(!evicted, "the 0 cursor is the before-anything baseline");
        assert_eq!(events.len(), 2, "only the retained window");
    }

    #[test]
    fn cursor_at_newest_returns_empty() {
        let mut buf = SurfacedEvents::new(10);
        buf.push(peer_return("a"));
        let s2 = buf.push(peer_return("b"));
        let (events, evicted) = buf.since(Some(s2));
        assert!(events.is_empty());
        assert!(!evicted);
    }

    #[test]
    fn cursor_at_evicted_seq_is_not_a_gap() {
        // Evicting the event *at* the cursor loses nothing: the caller already
        // consumed it, and the next expected seq is still present.
        let mut buf = SurfacedEvents::new(2);
        let consumed = buf.push(peer_return("a")); // seq 1, evicted next
        buf.push(peer_return("b")); // seq 2
        buf.push(peer_return("c")); // seq 3 → evicts seq 1; ring {2,3}
        let (events, evicted) = buf.since(Some(consumed));
        assert!(
            !evicted,
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
        let (events, evicted) = buf.since(Some(stale));
        assert!(
            evicted,
            "seq 2 aged out between the cursor and the retained window"
        );
        // Still returns the current window so the caller can re-baseline.
        assert_eq!(events.len(), 2);
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
        // Empty ring → register a waiter baselined at latest_seq() (0).
        assert!(
            surfaced
                .register_poll_waiter(
                    None,
                    TokioInstant::now() + Duration::from_secs(30),
                    PollResponder::Json(tx)
                )
                .is_none(),
            "registered, not degraded"
        );
        // A new event lands and the loop drains+fulfills.
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
        surfaced.register_poll_waiter(None, past, PollResponder::Json(tx));
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
        for _ in 0..agent_habilis_gossip::util::consts::POLL_WAITERS_CAP {
            let (tx, _rx) = tokio::sync::oneshot::channel::<String>();
            assert!(
                surfaced
                    .register_poll_waiter(None, deadline, PollResponder::Json(tx))
                    .is_none()
            );
        }
        // One more must be handed back (degrade), not parked.
        let (tx, _rx) = tokio::sync::oneshot::channel::<String>();
        assert!(
            surfaced
                .register_poll_waiter(None, deadline, PollResponder::Json(tx))
                .is_some(),
            "over the cap → responder returned to caller"
        );
        assert_eq!(
            surfaced.poll_waiters.len(),
            agent_habilis_gossip::util::consts::POLL_WAITERS_CAP
        );
    }

    #[tokio::test]
    async fn poll_or_register_responds_immediately_when_buffered() {
        let mut surfaced = SurfacedState::new();
        surfaced.push(peer_return("a"));
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        // Buffer has events → respond now even though long is set; no waiter.
        surfaced.poll_or_register(None, true, TokioInstant::now(), PollResponder::Json(tx));
        let body = rx.await.expect("immediate response");
        assert!(body.len() > 2, "non-empty: {body}");
        assert!(surfaced.poll_waiters.is_empty(), "never parked");
    }

    #[tokio::test]
    async fn poll_or_register_immediate_empty_when_not_long() {
        let mut surfaced = SurfacedState::new();
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        // Empty buffer, not long → immediate empty, no waiter.
        surfaced.poll_or_register(None, false, TokioInstant::now(), PollResponder::Json(tx));
        assert_eq!(rx.await.expect("immediate"), "[]");
        assert!(surfaced.poll_waiters.is_empty());
    }

    #[tokio::test]
    async fn poll_or_register_long_parks_then_expires_at_cap() {
        let mut surfaced = SurfacedState::new();
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        let now = TokioInstant::now();
        // Empty buffer, long → parked with deadline now + longpoll_max_ms().
        surfaced.poll_or_register(None, true, now, PollResponder::Json(tx));
        assert_eq!(surfaced.poll_waiters.len(), 1, "parked");
        let cap = Duration::from_millis(agent_habilis_gossip::util::tuning::longpoll_max_ms());
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
