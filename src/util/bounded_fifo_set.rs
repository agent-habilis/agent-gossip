//! A fixed-capacity set with FIFO eviction: the bounded building block for
//! long-lived state sets (`EventLoopState::known_endpoints`, `quiet`) so they
//! cannot grow without limit under churn / a sybil stream of one-shot ids.
//!
//! Pairs a `HashSet` (membership / O(1) lookup) with a `VecDeque` (insertion
//! order / eviction). Re-inserting an existing entry is a no-op (no recency
//! bump — recency isn't load-bearing for either caller, only "have we seen
//! this"). `MessageId` dedup keeps its own
//! [`BoundedIdSet`](crate::daemon::bounded_id_set::BoundedIdSet) (a
//! `was-duplicate`-returning variant).

use std::borrow::Borrow;
use std::collections::{HashSet, VecDeque};
use std::hash::Hash;

pub(crate) struct BoundedFifoSet<T> {
    capacity: usize,
    set: HashSet<T>,
    order: VecDeque<T>,
}

impl<T: Clone + Eq + Hash> BoundedFifoSet<T> {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            set: HashSet::new(),
            order: VecDeque::new(),
        }
    }

    /// Insert `item`; `true` if newly added, `false` if already present.
    /// Evicts the oldest entry when this pushes past `capacity`.
    pub(crate) fn insert(&mut self, item: T) -> bool {
        if !self.set.insert(item.clone()) {
            return false;
        }
        self.order.push_back(item);
        if self.order.len() > self.capacity
            && let Some(oldest) = self.order.pop_front()
        {
            self.set.remove(&oldest);
        }
        true
    }

    /// Remove `item`, keeping the eviction queue consistent. `true` if present.
    pub(crate) fn remove<Q>(&mut self, item: &Q) -> bool
    where
        T: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        if self.set.remove(item) {
            self.order.retain(|queued| queued.borrow() != item);
            true
        } else {
            false
        }
    }

    pub(crate) fn contains<Q>(&self, item: &Q) -> bool
    where
        T: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.set.contains(item)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &T> {
        self.set.iter()
    }

    pub(crate) fn len(&self) -> usize {
        self.set.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::BoundedFifoSet;

    #[test]
    fn insert_reports_novelty_and_dedupes() {
        let mut set = BoundedFifoSet::new(8);
        assert!(set.insert(1u32), "first insert is new");
        assert!(!set.insert(1u32), "re-insert is a dup, no-op");
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn evicts_oldest_past_capacity() {
        let mut set = BoundedFifoSet::new(3);
        for value in 0..3u32 {
            set.insert(value);
        }
        // Pushing a 4th evicts the oldest (0), FIFO; length stays capped.
        assert!(set.insert(99));
        assert_eq!(set.len(), 3);
        assert!(!set.contains(&0), "oldest evicted");
        assert!(set.contains(&99) && set.contains(&2));
    }

    #[test]
    fn stays_capped_under_churn() {
        let mut set = BoundedFifoSet::new(10);
        for value in 0..1000u32 {
            set.insert(value);
        }
        assert_eq!(set.len(), 10, "never exceeds capacity");
    }

    #[test]
    fn remove_clears_set_and_order() {
        let mut set = BoundedFifoSet::new(8);
        set.insert(1u32);
        set.insert(2u32);
        assert!(set.remove(&1));
        assert!(!set.contains(&1));
        assert!(!set.remove(&1), "second remove is a no-op");
        // Re-inserting the removed value works (order queue stayed consistent).
        assert!(set.insert(1u32));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn remove_by_borrowed_form() {
        // The `Borrow<Q>` bound lets a `String` set be queried/removed by `&str`.
        let mut set: BoundedFifoSet<String> = BoundedFifoSet::new(4);
        set.insert("alice".to_owned());
        assert!(set.contains("alice"));
        assert!(set.remove("alice"));
        assert!(set.is_empty());
    }
}
