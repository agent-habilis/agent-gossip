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
pub struct EventReceiver<T> {
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
            .expect("a panic while the feed lock is held would poison it, so the lock is never taken across one");
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
            .expect("a panic while the feed lock is held would poison it, so the lock is never taken across one")
            .pop_front()
    }

    pub async fn recv(&mut self) -> Option<T> {
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

    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
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

        fn events_lost(count: u64) -> Self {
            usize::try_from(count).expect("a test count fits in usize")
        }
    }

    #[derive(Debug, PartialEq)]
    enum Feed {
        Found(usize),
        Lost(usize),
        EventsLost(u64),
    }

    impl Evictable for Feed {
        fn is_lost(&self) -> bool {
            matches!(self, Feed::Lost(_))
        }

        fn events_lost(count: u64) -> Self {
            Feed::EventsLost(count)
        }
    }

    /// A consumer that lost events is told how many, before the next event, so it can list again.
    #[test]
    fn a_full_feed_tells_the_consumer_how_many_it_lost() {
        let (sink, mut rx) = event_feed::<Feed>();
        for index in 0..EVENT_FEED_CAP {
            sink.publish(Feed::Lost(index));
        }
        sink.publish(Feed::Found(9999));
        assert_eq!(
            rx.try_recv().expect("the marker comes first"),
            Feed::EventsLost(1),
            "one event was lost, and the marker says so before the next event"
        );
        assert_eq!(
            rx.try_recv()
                .expect("the next real event follows the marker"),
            Feed::Lost(1),
            "the event after the marker is the oldest that was kept"
        );
    }

    /// Without a drop, the stream carries no marker: the stream a consumer reads does not change.
    #[test]
    fn a_feed_with_no_drop_sends_no_marker() {
        let (sink, mut rx) = event_feed::<Feed>();
        sink.publish(Feed::Found(1));
        sink.publish(Feed::Found(2));
        assert_eq!(rx.try_recv().expect("the first event"), Feed::Found(1));
        assert_eq!(rx.try_recv().expect("the second event"), Feed::Found(2));
        assert!(rx.try_recv().is_err(), "no marker when nothing was dropped");
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
            "a consumer that misses a Lost keeps a mesh that is gone"
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
        let newest: Vec<usize> = (9 * EVENT_FEED_CAP..10 * EVENT_FEED_CAP).collect();
        assert_eq!(held, newest, "the feed holds the newest events, in order");
        assert_eq!(
            sink.dropped(),
            u64::try_from(9 * EVENT_FEED_CAP).expect("9 times the cap fits in u64"),
            "every event past the cap is evicted and counted"
        );
    }

    /// A full feed of `Lost` events still evicts, the oldest first, so the feed stays bounded.
    #[test]
    fn a_feed_of_lost_events_evicts_the_oldest() {
        let (sink, mut rx) = event_feed::<Feed>();
        for index in 0..=EVENT_FEED_CAP {
            sink.publish(Feed::Lost(index));
        }
        let first = rx
            .try_recv()
            .expect("publishing past the cap leaves at least one event to receive");
        assert_eq!(first, Feed::Lost(1), "the oldest Lost is the one evicted");
    }
}
