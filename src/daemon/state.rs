use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::time::Instant as TokioInstant;

use bytes::Bytes;
use iroh::EndpointId;

use super::bounded_id_set::BoundedIdSet;
use super::message_log::MessageLog;
use super::rate_limit::SwarmRateLimiter;
use crate::daemon::state_file::StateFile;
use crate::output;
use crate::protocol::identity::Identity;
use crate::protocol::{Message, MessageId, Nickname};

use crate::util::tuning::{
    KNOWN_ENDPOINTS_CAP, PENDING_OUTBOUND_CAP, RELINK_COOLDOWN_SECS, message_log_size, seen_ids_cap,
};

/// `RELINK_COOLDOWN_SECS` as a `Duration` — the single window value both
/// cooldown helpers compare against.
const RELINK_COOLDOWN: Duration = Duration::from_secs(RELINK_COOLDOWN_SECS);

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
    /// Per-endpoint re-link cooldown: the last `Instant` we re-dialed +
    /// re-flooded a peer learned via `PeerInfo`. Kept *across* `NeighborDown`
    /// (unlike `linked_endpoints`), so a flapping/unstable peer is re-linked at
    /// most once per `RELINK_COOLDOWN_SECS` — the choke that stops one bad
    /// node's flap from amplifying into a mesh-wide connection storm. Bounded:
    /// `note_relink` prunes expired entries, so it never outgrows the peers
    /// re-linked within the active window.
    pub relink_at: HashMap<EndpointId, Instant>,
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
    /// (GRAFT/repair, topology churn, our own re-broadcasts, anti-entropy
    /// re-sends, the rendezvous double-path) can deliver the same message
    /// twice; `mark_seen` drops the repeat before it reaches the log /
    /// embed channel / agent. Bounded (`seen_ids_cap`, 2× the message log)
    /// so it always covers the retention window.
    pub seen: BoundedIdSet,
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
    /// Rolling start index for the anti-entropy digest window: each round
    /// advertises `message_log[digest_cursor ..]` (up to
    /// `ANTIENTROPY_DIGEST_MAX_IDS`), then advances/wraps so a log larger
    /// than one digest is swept over several rounds.
    pub digest_cursor: usize,
    pub rate_limiter: SwarmRateLimiter,
    /// This member's signing identity (Ed25519). Shared with the
    /// send path so messages we author are signed before broadcast.
    /// The public key is the durable identity; the nickname is a
    /// non-unique display label (see `docs/history-integrity.md`).
    pub identity: Arc<Identity>,
    /// Our own per-author log cursor (Phase 2): `self_seq` is the next
    /// `Msg` sequence number to emit, `self_prev` the content hash of our
    /// last sent `Msg` (`None` until the first). Advanced on every send so
    /// our `Msg` stream is a `seq`+`prev` hash chain.
    pub self_seq: u64,
    pub self_prev: Option<String>,
    /// Phase 2 fork detection: per author pubkey (hex), the content hash
    /// seen at each `Msg` `seq`. A *different* hash at an already-seen
    /// `(pubkey, seq)` is cryptographic proof of equivocation → a `fork`
    /// event. Order-independent (gossip delivers out of order).
    pub author_seqs: HashMap<String, HashMap<u64, String>>,
    /// Author pubkeys already flagged as forked, so one `fork` event fires
    /// per offending key rather than per message.
    pub forked: HashSet<String>,
    /// Cross-author DAG (Phase 3): `by_hash` maps each known `Msg`'s content
    /// hash to its timestamp (for the `ts ≥ max(parents.ts)` backdating
    /// rule); `dag_heads` is the current tip set (hashes with no observed
    /// child) — the `parents` stamped on the next `Msg` we author. Both are
    /// pruned alongside the message-log eviction.
    pub by_hash: HashMap<String, i64>,
    pub dag_heads: HashSet<String>,
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
    /// tests can pin a deterministic instant; `rate_limit_per_min` is the
    /// swarm-wide cap decoded from the id (`0` ⇒ no limit).
    pub(crate) fn new(
        state_file: Option<StateFile>,
        now: Instant,
        rate_limit_per_min: u16,
        identity: Arc<Identity>,
    ) -> Self {
        Self {
            linked_endpoints: HashSet::new(),
            known_endpoints: HashSet::new(),
            known_order: VecDeque::new(),
            relink_at: HashMap::new(),
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
            seen: BoundedIdSet::new(seen_ids_cap()),
            pending_outbound: VecDeque::new(),
            state_file,
            live_count: None,
            message_log: MessageLog::new(message_log_size()),
            digest_cursor: 0,
            rate_limiter: SwarmRateLimiter::from_per_min(rate_limit_per_min),
            identity,
            self_seq: 0,
            self_prev: None,
            author_seqs: HashMap::new(),
            forked: HashSet::new(),
            by_hash: HashMap::new(),
            dag_heads: HashSet::new(),
            ping_round: None,
        }
    }

    /// The buffered messages after `after`, join-horizon filtered (never
    /// surfaces a message stamped before this process joined). The single
    /// source of truth for both the CLI socket `poll` and the typed
    /// in-process `Poll` (embed `fetch` / MCP `fetch_messages`). Emits the
    /// evicted-cursor notice through `output` when `after` aged out.
    pub(crate) fn poll_after(
        &self,
        after: Option<&MessageId>,
        output: &output::Output,
    ) -> Vec<Message> {
        let (mut messages, evicted) = self.message_log.messages_after(after);
        if evicted {
            output.info("poll: --after ID was evicted from buffer, returning all messages");
        }
        messages.retain(|message| message.timestamp >= self.joined_at);
        // Cap the response to the fixed IPC window (independent of the
        // configurable log size). Normally a no-op — the default log equals
        // the window — but a larger configured log surfaces only the most
        // recent `POLL_RESPONSE_MAX_MSGS` here (deep history still backs
        // anti-entropy recovery).
        if messages.len() > ahs_shared::POLL_RESPONSE_MAX_MSGS {
            let drop_count = messages.len() - ahs_shared::POLL_RESPONSE_MAX_MSGS;
            messages.drain(0..drop_count);
            output
                .info("poll: log exceeds the response window, returning the most recent messages");
        }
        tracing::debug!(returned = messages.len(), evicted, "poll served");
        messages
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

    /// `true` if we re-dialed + re-flooded `peer` within the last
    /// `RELINK_COOLDOWN_SECS` of `now`, so the caller should skip re-linking
    /// it again. Breaks the flap → re-dial → re-flood loop that otherwise
    /// turns one unstable peer into a mesh-wide CPU storm.
    pub(crate) fn relink_on_cooldown(&self, peer: EndpointId, now: Instant) -> bool {
        self.relink_at
            .get(&peer)
            .is_some_and(|at| now.duration_since(*at) < RELINK_COOLDOWN)
    }

    /// Record a re-link of `peer` at `now`, opportunistically dropping expired
    /// entries so the map stays bounded by the peers re-linked within the
    /// active window.
    pub(crate) fn note_relink(&mut self, peer: EndpointId, now: Instant) {
        self.relink_at
            .retain(|_, at| now.duration_since(*at) < RELINK_COOLDOWN);
        self.relink_at.insert(peer, now);
    }

    /// Record `id` as seen and report whether it was *already* seen.
    /// `true` => this is a duplicate delivery the caller must drop.
    /// Delegates to the bounded [`BoundedIdSet`].
    pub(crate) fn mark_seen(&mut self, id: &MessageId) -> bool {
        self.seen.mark(id)
    }

    /// Record a `Msg`'s `(pubkey, seq, content-hash)` for fork detection.
    /// Returns `true` **exactly once** per offending key — when a *different*
    /// content hash is first seen at an already-recorded `(pubkey, seq)`,
    /// which is cryptographic proof the author equivocated (signed two
    /// conflicting messages at one seq). Order-independent. The caller emits
    /// a `fork` event on `true`; the message itself is still processed.
    pub(crate) fn note_msg_seq(&mut self, pubkey: &str, seq: u64, hash: String) -> bool {
        let seen = self.author_seqs.entry(pubkey.to_owned()).or_default();
        match seen.get(&seq) {
            Some(existing) if *existing != hash => {} // conflict → fall through
            Some(_) => return false,                  // same message, already recorded
            None => {
                seen.insert(seq, hash);
                return false;
            }
        }
        // Equivocation: flag the key, returning true only on first detection.
        self.forked.insert(pubkey.to_owned())
    }

    /// The `parents` to stamp on the next `Msg` we author: the current DAG
    /// tips, sorted (deterministic) and capped to [`MAX_DAG_PARENTS`] to
    /// bound message size. Usually a single head on a quiet swarm.
    #[must_use]
    pub(crate) fn dag_parents(&self) -> Vec<String> {
        let mut heads: Vec<String> = self.dag_heads.iter().cloned().collect();
        heads.sort_unstable();
        heads.truncate(MAX_DAG_PARENTS);
        heads
    }

    /// Fold a content `Msg` (hash `hash`, `parents`, timestamp `ts`) into the
    /// DAG — for messages we receive *and* ones we author. Returns `true` if
    /// `ts` is **before** a known parent's timestamp (a backdating violation
    /// to flag). Updates `by_hash` and moves the tip set: the named parents
    /// are no longer tips, and `hash` becomes one. Unknown parents are left
    /// alone (the set converges via anti-entropy).
    pub(crate) fn note_dag(&mut self, hash: String, parents: &[String], ts: i64) -> bool {
        let mut backdated = false;
        for parent in parents {
            if let Some(&parent_ts) = self.by_hash.get(parent)
                && ts < parent_ts
            {
                backdated = true;
            }
            self.dag_heads.remove(parent);
        }
        self.by_hash.insert(hash.clone(), ts);
        self.dag_heads.insert(hash);
        backdated
    }

    /// Prune an evicted message's content hash from the DAG indexes, keeping
    /// `by_hash`/`dag_heads` bounded alongside the message-log `VecDeque`.
    pub(crate) fn forget_hash(&mut self, hash: &str) {
        self.by_hash.remove(hash);
        self.dag_heads.remove(hash);
    }

    /// Prune an evicted `Msg`'s `(pubkey, seq)` from the fork-detection
    /// index, bounding `author_seqs`/`forked` to the message-log window: an
    /// identity whose messages have all aged out drops off both maps (so a
    /// sybil that fires once and vanishes is forgotten).
    pub(crate) fn forget_msg_seq(&mut self, pubkey: &str, seq: u64) {
        if let Some(seqs) = self.author_seqs.get_mut(pubkey) {
            seqs.remove(&seq);
            if seqs.is_empty() {
                self.author_seqs.remove(pubkey);
                self.forked.remove(pubkey);
            }
        }
    }
}

/// Max `parents` stamped on a `Msg` — bounds the wire size of the causal
/// links. A quiet swarm has one head; this only bites under heavy concurrency.
const MAX_DAG_PARENTS: usize = 16;

#[cfg(test)]
mod tests {
    use super::{
        Bytes, Duration, EndpointId, EventLoopState, Instant, KNOWN_ENDPOINTS_CAP, MessageId,
        PENDING_OUTBOUND_CAP, RELINK_COOLDOWN_SECS,
    };

    fn fresh_state() -> EventLoopState {
        EventLoopState::new(
            None,
            Instant::now(),
            ahs_shared::RATE_LIMIT_PER_MIN,
            std::sync::Arc::new(crate::protocol::identity::Identity::generate()),
        )
    }

    #[test]
    fn note_msg_seq_detects_equivocation_once() {
        let mut state = fresh_state();
        let key = "alice-key";
        // First sighting at a seq → recorded, no fork.
        assert!(!state.note_msg_seq(key, 0, "hashA".into()));
        // Same message again (same hash) → no fork.
        assert!(!state.note_msg_seq(key, 0, "hashA".into()));
        // Different hash at the SAME seq → fork, reported exactly once.
        assert!(
            state.note_msg_seq(key, 0, "hashB".into()),
            "conflict is a fork"
        );
        assert!(
            !state.note_msg_seq(key, 0, "hashC".into()),
            "already flagged: one event per key"
        );
    }

    #[test]
    fn note_msg_seq_is_per_key_and_per_seq() {
        let mut state = fresh_state();
        assert!(!state.note_msg_seq("alice", 0, "h0".into()));
        // A new seq for the same author is just the next entry, not a fork.
        assert!(!state.note_msg_seq("alice", 1, "h1".into()));
        // A different key reusing seq 0 with its own hash is independent.
        assert!(!state.note_msg_seq("bob", 0, "other".into()));
        // But a conflict on alice's seq 0 still fires.
        assert!(state.note_msg_seq("alice", 0, "h0-prime".into()));
    }

    #[test]
    fn forget_msg_seq_prunes_author_and_fork_flag() {
        let mut state = fresh_state();
        state.note_msg_seq("alice", 0, "h0".into());
        assert!(state.note_msg_seq("alice", 0, "h0-prime".into()), "fork");
        assert!(state.forked.contains("alice"));
        // Evicting alice's only seq drops her from both maps (bounded to log).
        state.forget_msg_seq("alice", 0);
        assert!(!state.author_seqs.contains_key("alice"));
        assert!(
            !state.forked.contains("alice"),
            "fork flag pruned with author"
        );
    }

    #[test]
    fn note_dag_moves_tip_set() {
        let mut state = fresh_state();
        // First message (no parents) becomes the sole tip.
        assert!(!state.note_dag("h1".into(), &[], 100));
        assert_eq!(state.dag_parents(), vec!["h1".to_string()]);
        // A child referencing h1 makes h1 no longer a tip; h2 is the new one.
        assert!(!state.note_dag("h2".into(), &["h1".to_string()], 101));
        assert_eq!(state.dag_parents(), vec!["h2".to_string()]);
    }

    #[test]
    fn note_dag_keeps_concurrent_tips() {
        let mut state = fresh_state();
        state.note_dag("a".into(), &[], 1);
        state.note_dag("b".into(), &[], 1); // concurrent: no shared parent
        assert_eq!(state.dag_parents(), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn note_dag_flags_backdating() {
        let mut state = fresh_state();
        state.note_dag("parent".into(), &[], 200);
        // Child claims an earlier timestamp than a parent it references.
        assert!(state.note_dag("child".into(), &["parent".to_string()], 100));
        // A forward timestamp is fine.
        assert!(!state.note_dag("ok".into(), &["parent".to_string()], 300));
    }

    #[test]
    fn forget_hash_prunes_dag() {
        let mut state = fresh_state();
        state.note_dag("h".into(), &[], 1);
        state.forget_hash("h");
        assert!(state.dag_parents().is_empty());
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
    fn mark_seen_delegates_dedup() {
        // Smoke test the `EventLoopState` → `BoundedIdSet` delegation; the
        // bounded-FIFO/eviction logic itself is covered in `bounded_id_set`.
        let mut state = fresh_state();
        let id = MessageId::random();
        assert!(!state.mark_seen(&id), "first sighting is not a duplicate");
        assert!(state.mark_seen(&id), "second sighting is a duplicate");
    }

    // Replicates the mesh-wide CPU runaway at its mechanism: a peer whose
    // transport link flaps (each `NeighborDown` drops it from
    // `linked_endpoints`, each re-learned `PeerInfo` re-passes the novelty
    // gate) must NOT re-dial + re-flood once per flap. See memory
    // `cpu-runaway-membership-amplifier`.
    #[test]
    fn relink_cooldown_caps_a_flapping_peer() {
        let peer = endpoint_id(7);
        let start = Instant::now();

        // Baseline — documents the BUG: the bare novelty gate (remove+insert)
        // re-links on EVERY flap, i.e. unbounded amplification.
        let mut bare_state = fresh_state();
        let mut bare = 0;
        for _ in 0..100u32 {
            bare_state.linked_endpoints.remove(&peer);
            if bare_state.linked_endpoints.insert(peer) {
                bare += 1;
            }
        }
        assert_eq!(
            bare, 100,
            "without a cooldown every flap re-links — the runaway"
        );

        // With the cooldown: 100 flaps inside one window collapse to ONE re-link.
        let mut cooled = fresh_state();
        let mut relinks = 0;
        for index in 0..100u32 {
            let now = start + Duration::from_millis(u64::from(index) * 10);
            cooled.linked_endpoints.remove(&peer);
            if !cooled.relink_on_cooldown(peer, now) && cooled.linked_endpoints.insert(peer) {
                cooled.note_relink(peer, now);
                relinks += 1;
            }
        }
        assert_eq!(relinks, 1, "the cooldown caps re-links to once per window");

        // Past the window a genuine re-link is allowed again (no permanent lockout).
        let later = start + Duration::from_secs(RELINK_COOLDOWN_SECS + 1);
        assert!(!cooled.relink_on_cooldown(peer, later));
    }

    #[test]
    fn relink_cooldown_is_per_peer_and_bounded() {
        let mut state = fresh_state();
        let start = Instant::now();
        let flapping = endpoint_id(7);
        let other = endpoint_id(8);

        // One peer's cooldown never gates a different peer.
        state.note_relink(flapping, start);
        assert!(state.relink_on_cooldown(flapping, start));
        assert!(!state.relink_on_cooldown(other, start));

        // Many distinct peers re-linked across more than a full window:
        // expired entries are pruned, so the map never accumulates them all.
        for seed in 0..50u8 {
            let when = start + Duration::from_secs(u64::from(seed));
            state.note_relink(endpoint_id(seed), when);
        }
        assert!(
            state.relink_at.len() <= usize::try_from(RELINK_COOLDOWN_SECS).unwrap() + 1,
            "expired cooldown entries are pruned (map stays bounded)"
        );
    }

    #[test]
    fn dedup_window_covers_the_retention_window() {
        use crate::util::tuning::{message_log_size, seen_ids_cap};

        // Invariant the whole design rests on: the dedup set must outlive
        // the message log. Anti-entropy re-broadcasts any message still in
        // the log; if its id had scrolled out of the dedup set the resend
        // would be reprocessed and **re-surfaced** to the agent.
        assert!(
            seen_ids_cap() >= message_log_size(),
            "dedup cap ({}) must cover the retention window ({})",
            seen_ids_cap(),
            message_log_size(),
        );

        let mut state = fresh_state();
        let earliest = MessageId::random();
        assert!(!state.mark_seen(&earliest));
        // Mark a full buffer's worth of later ids.
        for _ in 1..message_log_size() {
            state.mark_seen(&MessageId::random());
        }
        // The earliest is still within the dedup window, so an anti-entropy
        // resend of it (while it is still retained in the log) is dropped as
        // a duplicate rather than surfaced a second time.
        assert!(
            state.mark_seen(&earliest),
            "a resend of a still-retained message must be deduped, not re-surfaced"
        );
    }
}
