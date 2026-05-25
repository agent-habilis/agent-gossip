use std::collections::{HashSet, VecDeque};

use crate::protocol::MessageId;

/// A bounded, insertion-ordered set of message ids for duplicate
/// suppression — O(1) membership plus a FIFO eviction queue, capped at
/// `capacity`. Consolidates the former `seen_ids`/`seen_order` field pair
/// into one entity owned by `EventLoopState`.
///
/// The cap is sized (`tuning::seen_ids_cap`, 2× the message log) to cover
/// the whole retention window: anti-entropy re-broadcasts any message still
/// in the log, and a re-send whose id had scrolled out of this set would be
/// reprocessed and **re-surfaced**, so the dedup window must outlive the
/// retained messages.
pub(crate) struct BoundedIdSet {
    capacity: usize,
    ids: HashSet<MessageId>,
    order: VecDeque<MessageId>,
}

impl BoundedIdSet {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            ids: HashSet::new(),
            order: VecDeque::new(),
        }
    }

    /// Record `id` as seen and report whether it was *already* seen
    /// (`true` ⇒ a duplicate the caller must drop). Bounded FIFO: evicts
    /// the oldest id past `capacity`.
    pub(crate) fn mark(&mut self, id: &MessageId) -> bool {
        if self.ids.contains(id) {
            return true;
        }
        self.ids.insert(id.clone());
        self.order.push_back(id.clone());
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
    use crate::protocol::MessageId;

    #[test]
    fn reports_first_then_duplicate() {
        let mut seen = BoundedIdSet::new(8);
        let id = MessageId::random();
        assert!(!seen.mark(&id), "first sighting is not a duplicate");
        assert!(seen.mark(&id), "second sighting is a duplicate");
        assert!(seen.mark(&id), "still a duplicate on repeat");
    }

    #[test]
    fn distinct_ids_are_independent() {
        let mut seen = BoundedIdSet::new(8);
        let one = MessageId::random();
        let two = MessageId::random();
        assert!(!seen.mark(&one));
        assert!(!seen.mark(&two));
        assert!(seen.mark(&one));
        assert!(seen.mark(&two));
    }

    #[test]
    fn evicts_oldest_past_cap() {
        let mut seen = BoundedIdSet::new(4);
        let first = MessageId::random();
        assert!(!seen.mark(&first));
        for _ in 0..4 {
            assert!(!seen.mark(&MessageId::random()));
        }
        // `first` was evicted (5 distinct ids past a cap of 4), so it is no
        // longer considered seen.
        assert!(!seen.mark(&first), "evicted id is no longer a duplicate");
        assert!(seen.len() <= 4);
    }
}
