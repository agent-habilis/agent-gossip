use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;
use tokio::sync::mpsc::error::TryRecvError;

pub(crate) const EVENT_FEED_CAP: usize = 256;

/// An event that a full feed may evict. A `Lost` stays: a consumer that misses it keeps a mesh
/// that is gone, and no later event tells it so.
pub(crate) trait Evictable {
    fn is_lost(&self) -> bool;
}

struct Shared<T> {
    queue: Mutex<VecDeque<T>>,
    ready: Notify,
    closed: AtomicBool,
    dropped: AtomicU64,
}

pub(crate) struct EventSink<T> {
    shared: Arc<Shared<T>>,
}

/// The consumer side of a feed. Its own type, not a channel receiver, so a full feed can evict
/// the oldest event that is not a `Lost`, which a channel cannot do.
pub(crate) struct EventReceiver<T> {
    shared: Arc<Shared<T>>,
}

pub(crate) fn event_feed<T>() -> (EventSink<T>, EventReceiver<T>) {
    let shared = Arc::new(Shared {
        queue: Mutex::new(VecDeque::with_capacity(EVENT_FEED_CAP)),
        ready: Notify::new(),
        closed: AtomicBool::new(false),
        dropped: AtomicU64::new(0),
    });
    (
        EventSink {
            shared: Arc::clone(&shared),
        },
        EventReceiver { shared },
    )
}

impl<T: Evictable> EventSink<T> {
    pub(crate) fn publish(&self, event: T) {
        let mut queue = self
            .shared
            .queue
            .lock()
            .expect("the feed lock is never held across a panic");
        if queue.len() >= EVENT_FEED_CAP {
            let evicted = queue
                .iter()
                .position(|queued| !queued.is_lost())
                .unwrap_or(0);
            queue.remove(evicted);
            self.shared.dropped.fetch_add(1, Ordering::Relaxed);
        }
        queue.push_back(event);
        drop(queue);
        self.shared.ready.notify_one();
    }

    #[cfg(test)]
    pub(crate) fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }
}

impl<T> Drop for EventSink<T> {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        self.shared.ready.notify_one();
    }
}

impl<T> std::fmt::Debug for EventReceiver<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventReceiver").finish_non_exhaustive()
    }
}

impl<T> EventReceiver<T> {
    fn pop(&self) -> Option<T> {
        self.shared
            .queue
            .lock()
            .expect("the feed lock is never held across a panic")
            .pop_front()
    }

    pub(crate) async fn recv(&mut self) -> Option<T> {
        loop {
            if let Some(event) = self.pop() {
                return Some(event);
            }
            if self.shared.closed.load(Ordering::Acquire) {
                return self.pop();
            }
            self.shared.ready.notified().await;
        }
    }

    pub(crate) fn try_recv(&mut self) -> Result<T, TryRecvError> {
        if let Some(event) = self.pop() {
            return Ok(event);
        }
        if self.shared.closed.load(Ordering::Acquire) {
            Err(TryRecvError::Disconnected)
        } else {
            Err(TryRecvError::Empty)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{EVENT_FEED_CAP, Evictable, event_feed};

    impl Evictable for usize {
        fn is_lost(&self) -> bool {
            false
        }
    }

    #[derive(Debug, PartialEq)]
    enum Feed {
        Found(usize),
        Lost(usize),
    }

    impl Evictable for Feed {
        fn is_lost(&self) -> bool {
            matches!(self, Feed::Lost(_))
        }
    }

    /// A `Lost` published into a full feed still reaches the consumer: a consumer that misses it
    /// keeps a mesh that is gone.
    #[test]
    fn a_lost_event_survives_a_full_feed() {
        let (sink, mut rx) = event_feed::<Feed>();
        for index in 0..EVENT_FEED_CAP {
            sink.publish(Feed::Found(index));
        }
        sink.publish(Feed::Lost(7));
        let mut delivered = Vec::new();
        while let Ok(event) = rx.try_recv() {
            delivered.push(event);
        }
        assert!(
            delivered.contains(&Feed::Lost(7)),
            "the Lost of a full feed is delivered"
        );
    }

    /// A full feed evicts its oldest event first, keeps the newest `EVENT_FEED_CAP`, and counts
    /// every eviction.
    #[test]
    fn a_full_feed_evicts_the_oldest_and_counts_it() {
        let (sink, mut rx) = event_feed::<usize>();
        for index in 0..10 * EVENT_FEED_CAP {
            sink.publish(index);
        }
        let mut held = Vec::new();
        while let Ok(event) = rx.try_recv() {
            held.push(event);
        }
        assert_eq!(
            held,
            (9 * EVENT_FEED_CAP..10 * EVENT_FEED_CAP).collect::<Vec<_>>(),
            "the feed holds the newest events, in order"
        );
        assert_eq!(
            sink.dropped(),
            u64::try_from(9 * EVENT_FEED_CAP).expect("the cap fits in u64"),
            "every event past the cap is evicted and counted"
        );
    }

    /// A full feed of `Lost` events still evicts, the oldest first, so the feed stays bounded.
    #[test]
    fn a_feed_of_lost_events_evicts_the_oldest() {
        let (sink, mut rx) = event_feed::<Feed>();
        for index in 0..EVENT_FEED_CAP + 1 {
            sink.publish(Feed::Lost(index));
        }
        let first = rx.try_recv().expect("the feed holds events");
        assert_eq!(first, Feed::Lost(1), "the oldest Lost is the one evicted");
    }
}
