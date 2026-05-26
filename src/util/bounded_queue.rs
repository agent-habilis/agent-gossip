//! A fixed-capacity FIFO queue: pushing past `capacity` evicts (and returns)
//! the oldest item. The bounded building block for `pending_outbound` (the
//! while-unmeshed send backlog) so it can't grow without limit.
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

    /// Append `item`. Returns the evicted oldest item when this push would
    /// exceed `capacity`, else `None`.
    pub(crate) fn push(&mut self, item: T) -> Option<T> {
        let evicted = if self.queue.len() >= self.capacity {
            self.queue.pop_front()
        } else {
            None
        };
        self.queue.push_back(item);
        evicted
    }

    /// Take everything in FIFO order, leaving the queue empty.
    pub(crate) fn take(&mut self) -> VecDeque<T> {
        std::mem::take(&mut self.queue)
    }
}

#[cfg(test)]
mod tests {
    use super::BoundedQueue;

    #[test]
    fn push_evicts_oldest_past_capacity() {
        let mut queue = BoundedQueue::new(2);
        assert_eq!(queue.push(1u32), None);
        assert_eq!(queue.push(2), None);
        assert_eq!(queue.push(3), Some(1), "oldest evicted past the cap");
        assert_eq!(
            queue.take().into_iter().collect::<Vec<_>>(),
            vec![2, 3],
            "FIFO order, capped"
        );
    }

    #[test]
    fn take_empties_in_fifo_order() {
        let mut queue = BoundedQueue::new(8);
        queue.push("a");
        queue.push("b");
        assert_eq!(queue.take().into_iter().collect::<Vec<_>>(), vec!["a", "b"]);
        assert!(queue.take().is_empty(), "second take is empty");
    }

    #[test]
    fn stays_capped_under_churn() {
        let mut queue = BoundedQueue::new(10);
        for value in 0..1000u32 {
            queue.push(value);
        }
        assert_eq!(queue.take().len(), 10, "never exceeds capacity");
    }
}
