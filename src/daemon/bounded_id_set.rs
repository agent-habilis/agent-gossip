use std::collections::{HashSet, VecDeque};

/// A bounded, insertion-ordered set of 16-byte **dedup keys**
/// (`Message::dedup_key`, `SHA-256(pubkey ‖ id)[..16]`) for duplicate
/// suppression — O(1) membership plus a FIFO eviction queue, capped at
/// `capacity`. Consolidates the former `seen_ids`/`seen_order` field pair
/// into one entity owned by `EventLoopState`. Keying on the author-bound
/// dedup key (not the bare id) is what stops a forged message from
/// suppressing a victim's genuine one under a reused id.
///
/// The cap is sized (`tuning::seen_ids_cap`, 2× the message log) to cover
/// the whole retention window: anti-entropy re-broadcasts any message still
/// in the log, and a re-send whose key had scrolled out of this set would be
/// reprocessed and **re-surfaced**, so the dedup window must outlive the
/// retained messages.
pub(crate) struct BoundedIdSet {
    capacity: usize,
    ids: HashSet<[u8; 16]>,
    order: VecDeque<[u8; 16]>,
}

impl BoundedIdSet {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            ids: HashSet::new(),
            order: VecDeque::new(),
        }
    }

    /// Record `key` as seen and report whether it was *already* seen
    /// (`true` ⇒ a duplicate the caller must drop). Bounded FIFO: evicts
    /// the oldest key past `capacity`.
    pub(crate) fn mark(&mut self, key: [u8; 16]) -> bool {
        if self.ids.contains(&key) {
            return true;
        }
        self.ids.insert(key);
        self.order.push_back(key);
        if self.order.len() > self.capacity
            && let Some(oldest) = self.order.pop_front()
        {
            self.ids.remove(&oldest);
        }
        false
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        debug_assert_eq!(self.ids.len(), self.order.len());
        self.order.len()
    }
}

#[cfg(test)]
mod tests {
    use super::BoundedIdSet;

    fn key(seed: u8) -> [u8; 16] {
        [seed; 16]
    }

    #[test]
    fn reports_first_then_duplicate() {
        let mut seen = BoundedIdSet::new(8);
        assert!(!seen.mark(key(1)), "first sighting is not a duplicate");
        assert!(seen.mark(key(1)), "second sighting is a duplicate");
        assert!(seen.mark(key(1)), "still a duplicate on repeat");
    }

    #[test]
    fn distinct_ids_are_independent() {
        let mut seen = BoundedIdSet::new(8);
        assert!(!seen.mark(key(1)));
        assert!(!seen.mark(key(2)));
        assert!(seen.mark(key(1)));
        assert!(seen.mark(key(2)));
    }

    #[test]
    fn evicts_oldest_past_cap() {
        let mut seen = BoundedIdSet::new(4);
        assert!(!seen.mark(key(0)));
        for seed in 1..=4 {
            assert!(!seen.mark(key(seed)));
        }
        // `key(0)` was evicted (5 distinct keys past a cap of 4), so it is no
        // longer considered seen.
        assert!(!seen.mark(key(0)), "evicted key is no longer a duplicate");
        assert!(seen.len() <= 4);
    }
}
