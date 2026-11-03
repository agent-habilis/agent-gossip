use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::{self};

pub(crate) const EVENT_FEED_CAP: usize = 256;

pub(crate) struct EventSink<T> {
    tx: mpsc::Sender<T>,
    dropped: AtomicU64,
}

pub(crate) fn event_feed<T>() -> (EventSink<T>, mpsc::Receiver<T>) {
    let (tx, rx) = mpsc::channel(EVENT_FEED_CAP);
    (
        EventSink {
            tx,
            dropped: AtomicU64::new(0),
        },
        rx,
    )
}

impl<T> EventSink<T> {
    pub(crate) fn publish(&self, event: T) {
        if let Err(TrySendError::Full(_)) = self.tx.try_send(event) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[cfg(test)]
    pub(crate) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::{EVENT_FEED_CAP, event_feed};

    #[test]
    fn a_full_feed_drops_the_new_event_and_counts_it() {
        let (sink, rx) = event_feed::<usize>();
        for index in 0..10 * EVENT_FEED_CAP {
            sink.publish(index);
        }
        assert!(
            rx.len() <= EVENT_FEED_CAP,
            "held {} events, cap is {}",
            rx.len(),
            EVENT_FEED_CAP
        );
        assert_eq!(
            sink.dropped(),
            u64::try_from(9 * EVENT_FEED_CAP).expect("the cap fits in u64")
        );
    }
}
