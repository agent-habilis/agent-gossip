//! A fixed-capacity FIFO queue: pushing when full **rejects the new item**
//! (nothing is evicted). The bounded building block for `pending_outbound`
//! (the while-unmeshed send backlog) so it can't grow without limit.
//!
//! Rejecting the newest rather than evicting the oldest matters: the items are
//! a per-author send chain (`seq`/`prev` hash-linked) replayed in order on
//! connect. Dropping a *middle* item would orphan its `seq` and leave peers a
//! dangling `prev`/parent; keeping an unbroken oldest-first prefix does not.
//!
//! Distinct from [`super::bounded_fifo_set::BoundedFifoSet`]: this keeps
//! duplicates and order matters (every item is later replayed in FIFO order),
//! and it is drained whole rather than queried for membership.

use std::collections::VecDeque;

pub(crate) struct BoundedQueue<T> {
    capacity: usize,
    queue: VecDeque<T>,
}

impl<T> BoundedQueue<T> {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            queue: VecDeque::new(),
        }
    }

    /// Append `item` if there is room. Returns `true` if stored, `false` if
    /// the queue is already at `capacity` (the item is **rejected** and the
    /// existing oldest-first run is left intact — see the module docs for why
    /// reject-newest beats evict-oldest for an ordered send chain).
    pub(crate) fn push(&mut self, item: T) -> bool {
        if self.queue.len() >= self.capacity {
            return false;
        }
        self.queue.push_back(item);
        true
    }

    /// Take everything in FIFO order, leaving the queue empty.
    pub(crate) fn take(&mut self) -> VecDeque<T> {
        std::mem::take(&mut self.queue)
    }

    /// Free slots before [`push`](Self::push) starts rejecting. Used to admit a
    /// whole multipart body atomically (all parts or none), so a half-buffered
    /// body never reaches peers.
    pub(crate) fn remaining(&self) -> usize {
        self.capacity - self.queue.len()
    }
}

#[cfg(test)]
mod tests {
    use super::BoundedQueue;

    #[test]
    fn push_rejects_newest_when_full() {
        let mut queue = BoundedQueue::new(2);
        assert!(queue.push(1u32));
        assert!(queue.push(2));
        assert!(!queue.push(3), "rejected when full — nothing evicted");
        assert_eq!(
            queue.take().into_iter().collect::<Vec<_>>(),
            vec![1, 2],
            "the oldest-first prefix is retained intact",
        );
    }

    #[test]
    fn take_empties_in_fifo_order() {
        let mut queue = BoundedQueue::new(8);
        assert!(queue.push("a"));
        assert!(queue.push("b"));
        assert_eq!(queue.take().into_iter().collect::<Vec<_>>(), vec!["a", "b"]);
        assert!(queue.take().is_empty(), "second take is empty");
    }

    #[test]
    fn stays_capped_under_churn() {
        let mut queue = BoundedQueue::new(10);
        let mut stored = 0;
        for value in 0..1000u32 {
            if queue.push(value) {
                stored += 1;
            }
        }
        assert_eq!(stored, 10, "only a capacity's worth is ever accepted");
        assert_eq!(queue.take().len(), 10, "never exceeds capacity");
    }
}
