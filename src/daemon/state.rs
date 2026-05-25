use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use tokio::time::Instant as TokioInstant;

use bytes::Bytes;
use iroh::EndpointId;

use super::message_log::MessageLog;
use super::rate_limit::SwarmRateLimiter;
use crate::protocol::{MessageId, Nickname};
use crate::daemon::state_file::StateFile;
use ahs_shared::DEFAULT_MESSAGE_LOG_SIZE;

use crate::util::tuning::{KNOWN_ENDPOINTS_CAP, PENDING_OUTBOUND_CAP, SEEN_IDS_CAP};

/// All mutable state owned by the event loop.
///
/// Grouped into a single struct so handlers and timer ticks can take
/// `&mut EventLoopState` instead of each borrowing half a dozen
/// independent locals.
///
/// The peer-tracking fields are deliberately one-per-layer (see the
/// Concept Glossary in AGENTS.md): `linked_endpoints` (transport),
/// `participants` (membership roster), `surfaced` (presentation gate),
/// `quiet` (heartbeat-evicted). Never conflate them — they are keyed
/// differently (node id vs nickname) and have different lifetimes.
pub(crate) struct EventLoopState {
    /// Transport layer: the set of `EndpointId`s we hold a direct
    /// link to (exchanged `PeerInfo` with). Bounded by `max_peers`.
    /// Used to dedupe learning the same endpoint twice. Distinct from
    /// `participants` — links are asymmetric and node-id keyed; the
    /// roster is symmetric and nickname keyed.
    pub linked_endpoints: HashSet<EndpointId>,
    /// Re-bridge memory: every peer `EndpointId` we've ever linked to,
    /// kept *across* `NeighborDown` (unlike `linked_endpoints`). When a
    /// node loses all links because the rendezvous/relay is unreachable,
    /// the healer re-dials these directly — iroh still holds their cached
    /// addresses — so the re-bridge no longer depends on the rendezvous.
    /// Bounded FIFO via `remember_endpoint`; `known_order` is its
    /// eviction queue.
    pub known_endpoints: HashSet<EndpointId>,
    pub known_order: VecDeque<EndpointId>,
    /// Membership layer: the participant roster. Nickname-keyed set of
    /// other participants, feeding the state file's `participant_count`
    /// (`participants.len() + 1`). Excludes self. Driven by
    /// `joined`/`left` presence messages (they reach every participant
    /// via gossip; `NeighborUp`/`PeerInfo` are asymmetric for
    /// bootstrap links), by implicit heartbeats from any received
    /// message (self-heal for a missed Joined), and by the sweeper's
    /// eviction of participants gone silent past `ALIVE_TIMEOUT_SECS`.
    pub participants: HashSet<Nickname>,
    /// Implicit-heartbeat tracker: nickname -> last time we heard
    /// anything from that participant. Drives sweep-based eviction.
    pub last_seen: HashMap<Nickname, Instant>,
    /// Heartbeat layer: participants we've evicted as quiet (silent
    /// past `ALIVE_TIMEOUT_SECS`) but who may still reappear. Any
    /// message from a nickname in this set triggers a symmetric
    /// `peer_return` event and re-inclusion in `participants`.
    pub quiet: HashSet<Nickname>,
    /// Presentation layer: participants for whom we have *surfaced* an
    /// arrival (synthetic `joined`, real `Presence::Joined`, or
    /// `peer_return`). Gates departure surfacing so a participant whose
    /// join we never showed never produces a `went quiet` / `has left`
    /// line — keeps the join-horizon view symmetric. Presentation-only:
    /// the roster (`participants`) stays complete for
    /// anti-entropy/membership; `surfaced ⊆ participants` only governs
    /// what the operator/agent is shown.
    pub surfaced: HashSet<Nickname>,
    /// Last time we broadcast anything. Lets idle daemons suppress
    /// their `Alive` keepalive when they're already chatty.
    pub last_sent_at: Instant,
    /// Wall-clock unix seconds when this daemon started — its join
    /// instant. The node still receives & relays older messages
    /// (anti-entropy keeps the swarm's set uniform), but messages
    /// stamped before this are never *surfaced* (printed / `poll` /
    /// `fetch` / embed): the operator/agent view starts at join.
    pub joined_at: i64,
    /// Goes false when the receiver stream terminally ends; IPC
    /// keeps working for `msg` / `poll` after that.
    pub gossip_open: bool,
    /// Set once we've broadcast our arrival (`joined` + `PeerInfo`).
    /// The announce is deferred to the first `NeighborUp` so it isn't
    /// lost into an unconnected overlay; subsequent neighbors only get
    /// a (log-invisible) `PeerInfo` re-send.
    pub announced: bool,
    /// Set once we have a link to a *real participant* (a non-
    /// rendezvous `NeighborUp`). `announced` flips on any neighbor —
    /// including the rendezvous relay — which is too early to deliver
    /// user content (the relay path may not be converged). Outbound
    /// user messages are buffered until `meshed`, then flushed, so the
    /// first message after a join can't be a lost one-shot.
    pub meshed: bool,
    /// When `Some(deadline)` and not yet elapsed, the event loop runs
    /// a fast `beacon::ensure` burst (event-driven failover). Armed
    /// on `NeighborDown` — the beacon may have just died — so a
    /// survivor claims the freed rendezvous port in ~1s rather than
    /// waiting for the next 15s heal tick.
    pub reclaim_until: Option<Instant>,
    /// Recently-seen message ids, for duplicate suppression. Gossip
    /// (GRAFT/repair, topology churn, our own re-broadcasts, the
    /// rendezvous double-path) can deliver the same message twice;
    /// `mark_seen` drops the repeat before it reaches the log / embed
    /// channel / agent. `seen_order` is the FIFO eviction queue for the
    /// set, bounded by `SEEN_IDS_CAP`.
    pub seen_ids: HashSet<MessageId>,
    pub seen_order: VecDeque<MessageId>,
    /// User messages sent before we had a real-peer link (no gossip
    /// path yet — a bare `broadcast` would be a lost one-shot).
    /// Drained in FIFO order once `meshed` flips; bounded by
    /// `PENDING_OUTBOUND_CAP`.
    pub pending_outbound: VecDeque<Bytes>,
    pub state_file: Option<StateFile>,
    /// When advertising (`create --advertise`), the directory's
    /// re-broadcast task reads the live participant count from here.
    /// Mirrors `participant_count` (`participants.len() + 1`), refreshed
    /// alongside every `write_participant_count`. `None` for the common
    /// non-advertising case (no shared counter to maintain).
    pub live_count: Option<Arc<AtomicUsize>>,
    pub message_log: MessageLog,
    pub rate_limiter: SwarmRateLimiter,
    /// Active `ahs ping` round, if one is in flight. Armed by the
    /// `Ping` IPC command, filled by inbound `Pong`s, and finalized
    /// into a `ping_report` when its `deadline` elapses. One at a time:
    /// a fresh ping replaces any in-flight round. Boxed to keep the
    /// rarely-set round off the hot event-loop future's stack size.
    pub ping_round: Option<Box<PingRound>>,
}

/// An in-flight RTT round. `t1` is when the probe was broadcast;
/// `pongs` records each peer's local arrival instant so RTT is
/// `arrival - t1`; the round is emitted and cleared at `deadline`.
pub(crate) struct PingRound {
    pub t1: TokioInstant,
    pub deadline: TokioInstant,
    pub pongs: HashMap<Nickname, TokioInstant>,
}

impl EventLoopState {
    /// Build a fresh event-loop state. `now` is passed explicitly so
    /// tests can pin a deterministic instant.
    pub(crate) fn new(state_file: Option<StateFile>, now: Instant) -> Self {
        Self {
            linked_endpoints: HashSet::new(),
            known_endpoints: HashSet::new(),
            known_order: VecDeque::new(),
            participants: HashSet::new(),
            last_seen: HashMap::new(),
            quiet: HashSet::new(),
            surfaced: HashSet::new(),
            last_sent_at: now,
            joined_at: crate::util::clock::unix_secs(),
            gossip_open: true,
            announced: false,
            meshed: false,
            reclaim_until: None,
            seen_ids: HashSet::new(),
            seen_order: VecDeque::new(),
            pending_outbound: VecDeque::new(),
            state_file,
            live_count: None,
            message_log: MessageLog::new(DEFAULT_MESSAGE_LOG_SIZE),
            rate_limiter: SwarmRateLimiter::new(),
            ping_round: None,
        }
    }

    /// Write `participant_count = participants.len() + 1` (we add 1
    /// for self) to the state file, if configured, and mirror it into
    /// the advertise counter, if present. No-op for neither.
    pub(crate) fn write_participant_count(&self) {
        let count = self.participants.len() + 1;
        if let Some(sf) = self.state_file.as_ref() {
            sf.write(count);
        }
        if let Some(live) = self.live_count.as_ref() {
            live.store(count, Ordering::Relaxed);
        }
    }

    /// Take everything buffered while unmeshed, FIFO order, leaving the
    /// buffer empty — the caller broadcasts each now that a real link
    /// exists.
    pub(crate) fn take_pending_outbound(&mut self) -> VecDeque<Bytes> {
        std::mem::take(&mut self.pending_outbound)
    }

    /// Buffer an outbound message that has no gossip path yet. Bounded
    /// FIFO: drops (and returns) the evicted oldest payload when the
    /// buffer is already at `PENDING_OUTBOUND_CAP`, else `None`.
    pub(crate) fn queue_outbound(&mut self, bytes: Bytes) -> Option<Bytes> {
        let evicted = if self.pending_outbound.len() >= PENDING_OUTBOUND_CAP {
            self.pending_outbound.pop_front()
        } else {
            None
        };
        self.pending_outbound.push_back(bytes);
        evicted
    }

    /// Remember a peer endpoint for the rendezvous-independent re-bridge.
    /// Bounded FIFO: evicts the oldest id past `KNOWN_ENDPOINTS_CAP`. A
    /// re-seen id keeps its original position (no recency bump) — recency
    /// isn't load-bearing here, only "have we ever linked this peer".
    pub(crate) fn remember_endpoint(&mut self, id: EndpointId) {
        if !self.known_endpoints.insert(id) {
            return;
        }
        self.known_order.push_back(id);
        if self.known_order.len() > KNOWN_ENDPOINTS_CAP
            && let Some(oldest) = self.known_order.pop_front()
        {
            self.known_endpoints.remove(&oldest);
        }
    }

    /// Record `id` as seen and report whether it was *already* seen.
    /// `true` => this is a duplicate delivery the caller must drop.
    /// Bounded FIFO: evicts the oldest id past `SEEN_IDS_CAP`.
    pub(crate) fn mark_seen(&mut self, id: &MessageId) -> bool {
        if self.seen_ids.contains(id) {
            return true;
        }
        self.seen_ids.insert(id.clone());
        self.seen_order.push_back(id.clone());
        if self.seen_order.len() > SEEN_IDS_CAP
            && let Some(oldest) = self.seen_order.pop_front()
        {
            self.seen_ids.remove(&oldest);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Bytes, EndpointId, EventLoopState, Instant, KNOWN_ENDPOINTS_CAP, MessageId,
        PENDING_OUTBOUND_CAP, SEEN_IDS_CAP,
    };

    fn fresh_state() -> EventLoopState {
        EventLoopState::new(None, Instant::now())
    }

    #[test]
    fn queue_outbound_is_bounded_fifo() {
        let mut state = fresh_state();
        // Fill exactly to cap — nothing evicted yet.
        for index in 0..PENDING_OUTBOUND_CAP {
            let evicted =
                state.queue_outbound(Bytes::from(vec![u8::try_from(index % 256).unwrap()]));
            assert!(evicted.is_none(), "no eviction below cap");
        }
        assert_eq!(state.pending_outbound.len(), PENDING_OUTBOUND_CAP);
        // One past cap evicts the oldest (FIFO), length stays capped.
        let evicted = state.queue_outbound(Bytes::from_static(b"newest"));
        assert_eq!(evicted, Some(Bytes::from(vec![0u8])), "oldest evicted");
        assert_eq!(state.pending_outbound.len(), PENDING_OUTBOUND_CAP);
        assert_eq!(
            state.pending_outbound.back().map(Bytes::as_ref),
            Some(b"newest".as_ref()),
            "newest is at the back"
        );
    }

    /// A valid (curve-point) `EndpointId` derived deterministically from
    /// a seed — `EndpointId::from_bytes` rejects arbitrary bytes.
    fn endpoint_id(seed: u8) -> EndpointId {
        iroh::SecretKey::from_bytes(&[seed; 32]).public()
    }

    #[test]
    fn remember_endpoint_is_bounded_fifo() {
        let mut state = fresh_state();
        let first = endpoint_id(0);
        state.remember_endpoint(first);
        // Fill past the cap with distinct ids; `first` is oldest and is
        // evicted, but the set stays capped.
        for index in 0..KNOWN_ENDPOINTS_CAP {
            state.remember_endpoint(endpoint_id(u8::try_from(index + 1).unwrap()));
        }
        assert!(
            !state.known_endpoints.contains(&first),
            "oldest endpoint evicted past the cap"
        );
        assert!(state.known_endpoints.len() <= KNOWN_ENDPOINTS_CAP);
        assert_eq!(state.known_order.len(), state.known_endpoints.len());
    }

    #[test]
    fn remember_endpoint_dedupes() {
        let mut state = fresh_state();
        let id = endpoint_id(7);
        state.remember_endpoint(id);
        state.remember_endpoint(id);
        assert_eq!(state.known_endpoints.len(), 1, "re-membering is a no-op");
        assert_eq!(state.known_order.len(), 1);
    }

    #[test]
    fn mark_seen_reports_first_then_duplicate() {
        let mut state = fresh_state();
        let id = MessageId::random();
        assert!(!state.mark_seen(&id), "first sighting is not a duplicate");
        assert!(state.mark_seen(&id), "second sighting is a duplicate");
        assert!(state.mark_seen(&id), "still a duplicate on repeat");
    }

    #[test]
    fn mark_seen_distinct_ids_are_independent() {
        let mut state = fresh_state();
        let (one, two) = (MessageId::random(), MessageId::random());
        assert!(!state.mark_seen(&one));
        assert!(!state.mark_seen(&two));
        assert!(state.mark_seen(&one));
        assert!(state.mark_seen(&two));
    }

    #[test]
    fn mark_seen_evicts_oldest_past_cap() {
        let mut state = fresh_state();
        let first = MessageId::random();
        assert!(!state.mark_seen(&first));
        // Fill past the cap; `first` is the oldest and must be evicted.
        for _ in 0..SEEN_IDS_CAP {
            assert!(!state.mark_seen(&MessageId::random()));
        }
        assert!(
            !state.mark_seen(&first),
            "evicted id is treated as new again"
        );
        assert_eq!(state.seen_order.len(), state.seen_ids.len());
        assert!(state.seen_order.len() <= SEEN_IDS_CAP);
    }
}
