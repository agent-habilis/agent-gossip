use std::collections::VecDeque;

use crate::output::OutputEvent;

/// One surfaced event tagged with its daemon-local sequence number. The `seq`
/// is the `poll` / `fetch` cursor value; `event` renders to the live-stream
/// JSON line via [`crate::output::surfaced_event_json`].
#[derive(Debug, Clone)]
pub struct SurfacedEvent {
    pub seq: u64,
    pub event: OutputEvent,
}

/// A bounded, seq-ordered record of everything the daemon **surfaced** to the
/// operator/agent — the history `poll` / `fetch_messages` drain.
///
/// Deliberately distinct from [`super::message_log::MessageLog`]: that is the
/// cross-node anti-entropy buffer, whose retention is a deterministic function
/// of the message *set* (`eviction_key`) so every node agrees on what survives.
/// This buffer is **local**: a single monotonic `seq` records *surfacing order*
/// on this node, so one `--after <seq>` cursor walks chat, presence, exchange
/// legs, and the transient events (`ping_report`, `peer_timeout`/`return`,
/// `exchange_timeout`, `fork`) that never enter the message log. Mixing the two
/// would couple a local cursor to the cross-node eviction order and break
/// anti-entropy convergence — hence two buffers.
///
/// Oldest-drop on overflow: the front (lowest seq) is evicted first, so the
/// retained window is always the most-recently-surfaced `capacity` events.
pub(crate) struct SurfacedEvents {
    capacity: usize,
    next_seq: u64,
    events: VecDeque<SurfacedEvent>,
}

impl SurfacedEvents {
    pub(crate) fn new(capacity: usize) -> Self {
        SurfacedEvents {
            capacity,
            // seq 0 is reserved as the "before anything" cursor: the first
            // pushed event is seq 1, so `since(Some(0))` returns the whole
            // buffer without a spurious eviction signal.
            next_seq: 1,
            events: VecDeque::with_capacity(capacity.min(64)),
        }
    }

    /// Append an event, assigning it the next seq, evicting the oldest if over
    /// capacity. Returns the assigned seq.
    pub(crate) fn push(&mut self, event: OutputEvent) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.events.push_back(SurfacedEvent { seq, event });
        while self.events.len() > self.capacity {
            self.events.pop_front();
        }
        seq
    }

    /// The events surfaced after `after`, in seq order, plus an `evicted` flag.
    ///
    /// - `after == None` → the whole buffer (a first poll), `evicted == false`.
    /// - `after == Some(seq)` → every event with `seq > after`. `evicted` is
    ///   true when `after` precedes the oldest retained seq *and* is not 0 —
    ///   i.e. the cursor aged out, so the caller silently missed the gap and
    ///   should re-baseline (the `poll` handler emits an `info` notice, the
    ///   same contract as the message log's evicted-cursor path).
    ///
    /// A cursor at or past the newest seq returns an empty slice with
    /// `evicted == false` (caller is simply up to date).
    pub(crate) fn since(&self, after: Option<u64>) -> (Vec<SurfacedEvent>, bool) {
        let Some(after) = after else {
            return (self.events.iter().cloned().collect(), false);
        };
        let evicted = after != 0
            && self
                .events
                .front()
                .is_some_and(|oldest| after < oldest.seq.saturating_sub(1));
        let slice = self
            .events
            .iter()
            .filter(|item| item.seq > after)
            .cloned()
            .collect();
        (slice, evicted)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.events.len()
    }
}

#[cfg(test)]
mod tests {
    use super::SurfacedEvents;
    use crate::output::OutputEvent;
    use crate::protocol::Nickname;

    fn peer_return(nick: &str) -> OutputEvent {
        OutputEvent::PeerReturn {
            nickname: Nickname::from(nick),
        }
    }

    #[test]
    fn first_poll_returns_all_no_eviction() {
        let mut buf = SurfacedEvents::new(10);
        buf.push(peer_return("a"));
        buf.push(peer_return("b"));
        let (events, evicted) = buf.since(None);
        assert_eq!(events.len(), 2);
        assert!(!evicted);
        assert_eq!(events[0].seq, 1);
        assert_eq!(events[1].seq, 2);
    }

    #[test]
    fn cursor_returns_only_newer() {
        let mut buf = SurfacedEvents::new(10);
        let s1 = buf.push(peer_return("a"));
        buf.push(peer_return("b"));
        buf.push(peer_return("c"));
        let (events, evicted) = buf.since(Some(s1));
        assert!(!evicted);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 2);
        assert_eq!(events[1].seq, 3);
    }

    #[test]
    fn cursor_zero_returns_all_without_eviction() {
        let mut buf = SurfacedEvents::new(2);
        // Overflow so the front seq is > 1, proving seq 0 is never "evicted".
        for nick in ["a", "b", "c", "d"] {
            buf.push(peer_return(nick));
        }
        let (events, evicted) = buf.since(Some(0));
        assert!(!evicted, "the 0 cursor is the before-anything baseline");
        assert_eq!(events.len(), 2, "only the retained window");
    }

    #[test]
    fn cursor_at_newest_returns_empty() {
        let mut buf = SurfacedEvents::new(10);
        buf.push(peer_return("a"));
        let s2 = buf.push(peer_return("b"));
        let (events, evicted) = buf.since(Some(s2));
        assert!(events.is_empty());
        assert!(!evicted);
    }

    #[test]
    fn cursor_at_evicted_seq_is_not_a_gap() {
        // Evicting the event *at* the cursor loses nothing: the caller already
        // consumed it, and the next expected seq is still present.
        let mut buf = SurfacedEvents::new(2);
        let consumed = buf.push(peer_return("a")); // seq 1, evicted next
        buf.push(peer_return("b")); // seq 2
        buf.push(peer_return("c")); // seq 3 → evicts seq 1; ring {2,3}
        let (events, evicted) = buf.since(Some(consumed));
        assert!(
            !evicted,
            "cursor at the evicted seq still has seq 2 next — no gap"
        );
        assert_eq!(events.len(), 2, "returns seq 2 and 3");
    }

    #[test]
    fn evicted_cursor_flags_gap() {
        // A real gap: the cursor's next-expected seq was evicted unseen.
        let mut buf = SurfacedEvents::new(2);
        let stale = buf.push(peer_return("a")); // seq 1 (the cursor)
        buf.push(peer_return("b")); // seq 2 → evicted unseen
        buf.push(peer_return("c")); // seq 3
        buf.push(peer_return("d")); // seq 4 → evicts seq 2; ring {3,4}
        assert_eq!(buf.len(), 2);
        let (events, evicted) = buf.since(Some(stale));
        assert!(
            evicted,
            "seq 2 aged out between the cursor and the retained window"
        );
        // Still returns the current window so the caller can re-baseline.
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn oldest_drop_keeps_recent_window() {
        let mut buf = SurfacedEvents::new(3);
        for nick in ["a", "b", "c", "d", "e"] {
            buf.push(peer_return(nick));
        }
        assert_eq!(buf.len(), 3);
        let (events, _) = buf.since(None);
        let seqs: Vec<u64> = events.iter().map(|item| item.seq).collect();
        assert_eq!(seqs, vec![3, 4, 5], "kept the three newest by seq");
    }
}
