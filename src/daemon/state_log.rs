//! The durable swarm-state log: the signed `State` events the swarm agrees on,
//! and the deterministic fold that derives state from them.
//!
//! Unlike the chat [`MessageLog`](super::message_log::MessageLog), this store is
//! **un-pruned and unbounded** — state must outlive the chat retention window,
//! and a shared document may accumulate an arbitrary number of changes over a
//! swarm's life, so nothing is ever evicted. The store is dedup-keyed by message
//! id; a (re)joining member starts empty and reconstructs the full set from peers
//! via the state anti-entropy path, then replays it. (Bounding total growth for a
//! long-lived swarm is log compaction/snapshots — deferred future work.)
//!
//! Because the log is unbounded, its id set can outgrow a single gossip message,
//! so anti-entropy advertises it in **windows** ([`recent_window`](StateLog::recent_window)
//! / [`older_window`](StateLog::older_window), the `(timestamp, id)`-ordered
//! analogue of the chat digest) rather than one flat set, swept across rounds via
//! a cursor. A late joiner reconciles the whole log over several windows.
//!
//! The log never interprets an event's payload (`body`): it only guarantees a
//! complete, stable event set and a deterministic replay order. Turning that
//! into meaningful state is a [`StateProjection`]'s job (e.g.
//! [`state_doc`](super::state_doc) folds JSON-Patch changes into a document).

use std::collections::{HashMap, HashSet};

use crate::daemon::message_log::DigestWindow;
use crate::protocol::{Message, MessageId};

/// A fold of the state log into some derived state. [`StateLog::derive`] replays
/// every event in a deterministic order and applies each through this trait; the
/// implementor accumulates whatever projection it cares about.
pub(crate) trait StateProjection {
    fn apply(&mut self, event: &Message);
}

/// The un-pruned, unbounded store of signed `State` events, keyed by id for
/// dedup. Events never age out: a derived document is the fold over the complete
/// set, so dropping any event would diverge peers. Memory grows with total state
/// history (bounded in *rate* by the per-author limiter; bounding the *total* is
/// future compaction work).
#[derive(Debug, Default)]
pub(crate) struct StateLog {
    events: HashMap<MessageId, Message>,
}

impl StateLog {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record a state event. Returns `true` when it was newly stored, `false`
    /// for a duplicate id. No capacity bound — the fold needs the complete set.
    pub(crate) fn insert(&mut self, event: Message) -> bool {
        if self.events.contains_key(&event.id) {
            return false;
        }
        self.events.insert(event.id.clone(), event);
        true
    }

    pub(crate) fn len(&self) -> usize {
        self.events.len()
    }

    /// Held events in the deterministic `(timestamp, id)` order every honest
    /// member shares — so replay and resend order are stable across nodes.
    fn ordered(&self) -> Vec<&Message> {
        let mut ordered: Vec<&Message> = self.events.values().collect();
        ordered.sort_by(|left, right| {
            left.timestamp
                .cmp(&right.timestamp)
                .then_with(|| left.id.as_str().cmp(right.id.as_str()))
        });
        ordered
    }

    /// A single window covering the **entire** range, `[i64::MIN, i64::MAX]`,
    /// with every held id (empty when the log is empty). What a small or
    /// just-joined member advertises: a holder then re-sends every gap below,
    /// inside, and above the member's set — the full-history bootstrap an empty
    /// joiner needs (the chat digest, ephemeral, has no equivalent). Once the
    /// set outgrows one window, the digest switches to recent + rolling-older.
    pub(crate) fn bootstrap_window(&self) -> DigestWindow {
        let ids = self
            .ordered()
            .iter()
            .map(|event| event.id.as_uuid_bytes())
            .collect();
        DigestWindow {
            lo: i64::MIN,
            hi: i64::MAX,
            ids,
        }
    }

    /// The newest `recent` events as an **open-ended** window (`hi = i64::MAX`):
    /// "I hold everything from `lo` onward except the gaps not in `ids`", so a
    /// holder re-sends every newer event the member lacks. `None` only if the
    /// log is empty.
    pub(crate) fn recent_window(&self, recent: usize) -> Option<DigestWindow> {
        let ordered = self.ordered();
        let start = ordered.len().saturating_sub(recent);
        let mut window = window_of(&ordered[start..])?;
        window.hi = i64::MAX;
        Some(window)
    }

    /// Number of events older than the newest `recent` — the portion the
    /// rolling [`older_window`](Self::older_window) sweeps.
    pub(crate) fn older_len(&self, recent: usize) -> usize {
        self.events.len().saturating_sub(recent)
    }

    /// A rolling **closed** window over the older portion (everything before the
    /// newest `recent`): up to `max` ids starting at `start` *within* that
    /// portion, with exact `[lo, hi]` bounds so receivers reconcile deep
    /// interior gaps without re-sending the out-of-window remainder. `None` when
    /// there is no older portion (`len <= recent`). The caller opens the bottom
    /// (`lo = i64::MIN`) on the `start == 0` sweep so events *below* the member's
    /// oldest held event are pulled too — the full-history guarantee state needs.
    pub(crate) fn older_window(
        &self,
        recent: usize,
        start: usize,
        max: usize,
    ) -> Option<DigestWindow> {
        let older_len = self.older_len(recent);
        if older_len == 0 {
            return None;
        }
        let start = start % older_len;
        let count = max.min(older_len - start);
        let ordered = self.ordered();
        window_of(&ordered[start..start + count])
    }

    /// Up to `max` of our events within the `[lo, hi]` timestamp window whose
    /// compact id is **not** in `have` — the in-window gap to re-broadcast so a
    /// peer that advertised that window recovers what it missed. Returned
    /// **oldest-first** (the `(ts,id)` replay order), so a catching-up joiner
    /// fills from the bottom up and its open-ended recent window stays anchored
    /// at the genesis until it converges. Out-of-window events are never re-sent.
    pub(crate) fn missing_in_window(
        &self,
        lo: i64,
        hi: i64,
        have: &HashSet<[u8; 16]>,
        max: usize,
    ) -> Vec<&Message> {
        self.ordered()
            .into_iter()
            .filter(|event| {
                event.timestamp >= lo
                    && event.timestamp <= hi
                    && !have.contains(&event.id.as_uuid_bytes())
            })
            .take(max)
            .collect()
    }

    /// Replay every held event through `projection` in the deterministic order
    /// `(timestamp, id)` that every honest member shares — so all members derive
    /// the same state regardless of the order events arrived. (Strict
    /// causal/topological replay via `parents` is a follow-up for the first
    /// projection that needs author-chain ordering; the events already carry the
    /// links.)
    pub(crate) fn derive<P: StateProjection>(&self, projection: &mut P) {
        for event in self.ordered() {
            projection.apply(event);
        }
    }

    /// Event payloads in deterministic replay order — used to prove log
    /// convergence in tests.
    #[cfg(test)]
    pub(crate) fn replay_bodies(&self) -> Vec<String> {
        #[derive(Default)]
        struct Bodies(Vec<String>);
        impl StateProjection for Bodies {
            fn apply(&mut self, event: &Message) {
                self.0.push(event.body.as_str().to_owned());
            }
        }
        let mut bodies = Bodies::default();
        self.derive(&mut bodies);
        bodies.0
    }
}

/// Build a digest window from a `(ts,id)`-ordered slice: its `[lo, hi]` bounds
/// (the slice's first/last timestamps) and the compact ids. `None` if empty.
fn window_of(slice: &[&Message]) -> Option<DigestWindow> {
    let lo = slice.first()?.timestamp;
    let hi = slice.last()?.timestamp;
    let ids = slice.iter().map(|event| event.id.as_uuid_bytes()).collect();
    Some(DigestWindow { lo, hi, ids })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{StateLog, StateProjection};
    use crate::protocol::message::MessageBody;
    use crate::protocol::{Message, Nickname, SwarmId};

    /// A trivial projection: collect each event body in replay order, so the
    /// test can assert both completeness and deterministic ordering.
    #[derive(Default)]
    struct Collected {
        bodies: Vec<String>,
    }

    impl StateProjection for Collected {
        fn apply(&mut self, event: &Message) {
            self.bodies.push(event.body.as_str().to_owned());
        }
    }

    fn state_event(swarm: &SwarmId, author: &Nickname, ts: i64, body: &str) -> Message {
        let mut msg = Message::new_state(swarm, author, MessageBody::new(body).unwrap());
        msg.timestamp = ts;
        msg
    }

    fn fixture() -> (SwarmId, Nickname) {
        let swarm = SwarmId::new(
            "🐝e9MR9KQRSaraN3u5EbgwgoXqHJwQnyMzq6r4xGdch3jcr3CQtueAAeQ7AZrvYmjgUZpYP2jwd8",
        )
        .expect("valid swarm id");
        (swarm, Nickname::new("test-node").expect("valid nickname"))
    }

    #[test]
    fn derive_replays_in_timestamp_order_regardless_of_insert_order() {
        let (swarm, author) = fixture();
        let mut log = StateLog::new();
        // Insert out of timestamp order.
        log.insert(state_event(&swarm, &author, 30, "third"));
        log.insert(state_event(&swarm, &author, 10, "first"));
        log.insert(state_event(&swarm, &author, 20, "second"));

        let mut derived = Collected::default();
        log.derive(&mut derived);
        assert_eq!(derived.bodies, ["first", "second", "third"]);
    }

    #[test]
    fn insert_dedups_and_is_unbounded() {
        let (swarm, author) = fixture();
        let mut log = StateLog::new();
        let event = state_event(&swarm, &author, 1, "once");
        assert!(log.insert(event.clone()), "first insert is new");
        assert!(!log.insert(event.clone()), "same id is a duplicate");

        // Grow well past any former cap: the unbounded log keeps every event.
        let total = 500;
        for index in 0..total {
            log.insert(state_event(
                &swarm,
                &author,
                i64::try_from(index).unwrap() + 2,
                &format!("e{index}"),
            ));
        }
        assert_eq!(
            log.len(),
            total + 1,
            "unbounded log retains the genesis plus every later event"
        );
        assert!(log.replay_bodies().contains(&"once".to_string()));
    }

    #[test]
    fn windows_cover_below_inside_and_above_for_full_reconcile() {
        let (swarm, author) = fixture();
        let mut log = StateLog::new();
        // 10 events at ts 1..=10; window size 4 so there's a real older portion.
        for ts in 1..=10 {
            log.insert(state_event(&swarm, &author, ts, &format!("e{ts}")));
        }
        let recent = 4;

        // Recent window is open-ended at the top and holds the newest 4 ids.
        let newest = log.recent_window(recent).expect("non-empty");
        assert_eq!(newest.hi, i64::MAX, "recent window is open at the top");
        assert_eq!(newest.ids.len(), recent);

        // A peer that holds only the middle (ts 4..=7) advertised as a closed
        // window must be re-sent both the below (1..=3) and above (8..=10) gaps.
        let have: HashSet<[u8; 16]> = log
            .missing_in_window(4, 7, &HashSet::new(), usize::MAX)
            .iter()
            .map(|event| event.id.as_uuid_bytes())
            .collect();
        // Open-ended recent [lo=newest.lo, MAX]: everything from newest.lo up.
        let above = log.missing_in_window(newest.lo, i64::MAX, &have, usize::MAX);
        assert!(
            above.iter().any(|event| event.timestamp == 10),
            "open-top recent pulls the newest the peer lacks"
        );
        // Open-bottom older [MIN, hi]: everything below the peer's oldest.
        let below = log.missing_in_window(i64::MIN, 7, &have, usize::MAX);
        assert!(
            below.iter().any(|event| event.timestamp == 1),
            "open-bottom older pulls the oldest the peer lacks"
        );
        // missing_in_window returns oldest-first.
        let ordered = log.missing_in_window(i64::MIN, i64::MAX, &HashSet::new(), usize::MAX);
        let timestamps: Vec<i64> = ordered.iter().map(|event| event.timestamp).collect();
        assert_eq!(timestamps, (1..=10).collect::<Vec<_>>());
    }
}
