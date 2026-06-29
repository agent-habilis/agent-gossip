//! The durable swarm-state log: the signed `State` events the swarm agrees on,
//! and the deterministic fold that derives state from them.
//!
//! Unlike the chat [`MessageLog`](super::message_log::MessageLog), this store is
//! **un-pruned** — state must outlive the retention window, so nothing is ever
//! evicted. State changes are rare and small (membership edits), so unbounded
//! retention is cheap. The store is dedup-keyed by message id; a (re)joining
//! member starts empty and reconstructs the full set from peers via the state
//! anti-entropy path, then replays it.
//!
//! The log never interprets an event's payload (`body`): it only guarantees a
//! complete, stable event set and a deterministic replay order. Turning that
//! into meaningful state is a [`StateProjection`]'s job, and the
//! convergence/authority semantics (CRDT, auth-chains) live there — they arrive
//! with the first consumer (a future allowlist), not in this substrate.

use std::collections::{HashMap, HashSet};

use crate::protocol::{Message, MessageId};
use crate::util::tuning::STATE_LOG_CAP;

/// A fold of the state log into some derived state. [`StateLog::derive`] replays
/// every event in a deterministic order and applies each through this trait; the
/// implementor accumulates whatever projection it cares about.
pub(crate) trait StateProjection {
    fn apply(&mut self, event: &Message);
}

/// The un-pruned store of signed `State` events, keyed by id for dedup, with a
/// [`STATE_LOG_CAP`] memory bound (a denial-of-service safety valve, not normal
/// retention — see the const). "Un-pruned" means events never age out of a *retention
/// window* the way chat does; the cap is a flat ceiling reached only under
/// abuse or, for now, a swarm with an unusually high volume of state edits.
///
/// The cap is sized so the complete set of compact ids fits one dedicated
/// `StateDigest`, so the whole log is advertised/reconciled in one message with
/// no windowing. Scaling past one digest (windowed/cursored state anti-entropy,
/// or log compaction/snapshots) is a follow-up; today's consumers (a future
/// allowlist) sit comfortably under the cap.
#[derive(Debug)]
pub(crate) struct StateLog {
    events: HashMap<MessageId, Message>,
    cap: usize,
}

impl Default for StateLog {
    fn default() -> Self {
        Self {
            events: HashMap::new(),
            cap: STATE_LOG_CAP,
        }
    }
}

impl StateLog {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record a state event. Returns `true` when it was newly stored. A
    /// duplicate id, or any new event once the log is at `cap`, returns `false`
    /// — **reject-newest**, so established state is preserved (a flood can't
    /// evict the genesis) and memory + the advertised id set stay bounded.
    pub(crate) fn insert(&mut self, event: Message) -> bool {
        if self.events.contains_key(&event.id) {
            return false;
        }
        if self.events.len() >= self.cap {
            tracing::warn!(
                cap = self.cap,
                author = %event.author,
                "state log at capacity; dropping new state event (DoS bound)"
            );
            return false;
        }
        self.events.insert(event.id.clone(), event);
        true
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

    /// The complete set of held event ids, packed as raw 16-byte UUIDs and
    /// Base58-encoded — what a `StateDigest` advertises. ~22 chars/id vs 36 as a
    /// string; with the `STATE_LOG_CAP` bound the whole set fits one gossip
    /// message (the [former overflow](crate::gossip) the chat digest also guards
    /// against).
    pub(crate) fn packed_ids(&self) -> String {
        let mut packed = Vec::with_capacity(self.events.len() * 16);
        for id in self.events.keys() {
            packed.extend_from_slice(&id.as_uuid_bytes());
        }
        bs58::encode(packed).into_string()
    }

    /// The held events a peer's digest omits, in deterministic order, so a
    /// cold/late joiner (which advertises an empty set) backfills the whole log
    /// and the resend order is stable.
    pub(crate) fn missing_from(&self, have: &HashSet<[u8; 16]>) -> Vec<&Message> {
        self.ordered()
            .into_iter()
            .filter(|event| !have.contains(&event.id.as_uuid_bytes()))
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

    /// Event payloads in deterministic replay order — the substrate's generic
    /// read, used to prove convergence in tests and as a stand-in until typed
    /// projections (a future allowlist) land.
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

#[cfg(test)]
mod tests {
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
    fn insert_dedups_and_caps_reject_newest() {
        let (swarm, author) = fixture();
        let mut log = StateLog::new();
        let event = state_event(&swarm, &author, 1, "once");
        assert!(log.insert(event.clone()), "first insert is new");
        assert!(!log.insert(event.clone()), "same id is a duplicate");
        assert!(log.replay_bodies().contains(&"once".to_string()));

        // Flood well past the cap; the log stays bounded (DoS valve) and
        // reject-newest keeps the established "once" rather than evicting it.
        for index in 0..crate::util::tuning::STATE_LOG_CAP * 2 {
            log.insert(state_event(
                &swarm,
                &author,
                i64::try_from(index).unwrap() + 2,
                &format!("e{index}"),
            ));
        }
        assert_eq!(
            log.replay_bodies().len(),
            crate::util::tuning::STATE_LOG_CAP,
            "log is bounded at the cap"
        );
        assert!(
            log.replay_bodies().contains(&"once".to_string()),
            "reject-newest preserves established state under a flood"
        );
    }
}
