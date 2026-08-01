use std::collections::{HashSet, VecDeque};

use crate::protocol::Message;

/// One anti-entropy digest window: the inclusive `[lo, hi]` timestamp
/// range it covers and the compact (raw 16-byte UUID) ids the sender holds
/// in that range. The bounds let a receiver re-send only **in-window** gaps,
/// so advertising a sub-window of a large log never makes peers perpetually
/// re-send the out-of-window remainder.
pub(crate) struct DigestWindow {
    pub lo: i64,
    pub hi: i64,
    pub ids: Vec<[u8; 16]>,
}

/// Inclusive `[lo, hi]` timestamp bounds to filter by — the shape shared by a
/// [`DigestWindow`] and a wire-decoded digest window entry (the anti-entropy
/// caller's own type), grouped so [`MessageLog::missing_in_window`] doesn't
/// need either concrete type by name.
#[derive(Clone, Copy)]
pub(crate) struct WindowRange {
    pub lo: i64,
    pub hi: i64,
}

/// A bounded buffer of the recent messages a member retains — the
/// anti-entropy recovery source and the poll/fetch history. Held in arrival
/// order (push order ≈ ascending timestamp) for the poll cursor and the
/// positional digest windows. When full, the message with the smallest
/// [`eviction_key`] is discarded — *not* the front — so the **retained set**
/// is a deterministic function of the message set, identical on every node
/// regardless of gossip delivery order (see [`MessageLog::push`]).
#[derive(Debug)]
pub struct MessageLog {
    capacity: usize,
    messages: VecDeque<Message>,
}

impl MessageLog {
    pub(crate) fn new(capacity: usize) -> Self {
        MessageLog {
            capacity,
            messages: VecDeque::with_capacity(capacity),
        }
    }

    /// Add a message to the log, keeping arrival order. If that overflows
    /// the capacity, evict (and return) the message with the smallest
    /// **eviction key** — *not* the front — so the retained set is a
    /// deterministic function of `(message set, capacity)`, identical on
    /// every node regardless of gossip delivery order. That makes the
    /// mesh-wide retained set well-defined: peers agree on which messages
    /// survive, so anti-entropy recovery converges on one set instead of the
    /// union of divergent arrival-order windows. The returned eviction lets
    /// callers prune side indexes keyed by it (the DAG `by_hash`, fork map).
    pub fn push(&mut self, msg: Message) -> Option<Message> {
        self.messages.push_back(msg);
        if self.messages.len() <= self.capacity {
            return None;
        }
        let victim = self
            .messages
            .iter()
            .enumerate()
            .min_by(|(_, lhs), (_, rhs)| eviction_key(lhs).cmp(&eviction_key(rhs)))
            .map(|(index, _)| index)
            .expect("over-capacity log is non-empty");
        self.messages.remove(victim)
    }

    pub(crate) fn len(&self) -> usize {
        self.messages.len()
    }

    /// The retained shards of multipart `group` authored by `pubkey`, slotted by
    /// `idx` (first-seen wins per idx); `total` slots, each `None` until its shard
    /// arrives. Keying on `pubkey` as well as `group` is the security boundary: a
    /// peer can only sign shards under its own key, so a crafted shard carrying
    /// someone else's `group` forms a separate, never-completing set and can't
    /// inject a slice into the victim's body. Only **small** (logged) groups ever
    /// reach this — `total` is bounded by `LOGGED_SHARD_GROUP_MAX_TOTAL` at the
    /// call site, so the `total`-sized allocation is safe.
    pub(crate) fn collect_shards<'a>(
        &'a self,
        group: &crate::protocol::ShardGroup,
        pubkey: &str,
        total: u32,
    ) -> Vec<Option<&'a Message>> {
        let mut slots: Vec<Option<&Message>> = vec![None; total as usize];
        for msg in &self.messages {
            let Some(shard) = &msg.shard else { continue };
            if shard.group == *group
                && shard.total == total
                && msg.pubkey == pubkey
                && let Some(slot) = slots.get_mut(shard.idx as usize)
                && slot.is_none()
            {
                *slot = Some(msg);
            }
        }
        slots
    }

    /// A contiguous window of the log: up to `max` messages starting at
    /// index `start`, with their inclusive `[lo, hi]` timestamp bounds and
    /// compact ids. `None` if the log is empty or `start` is past the end.
    pub(crate) fn window_at(&self, start: usize, max: usize) -> Option<DigestWindow> {
        let slice: Vec<&Message> = self.messages.iter().skip(start).take(max).collect();
        let lo = slice.first()?.timestamp;
        let hi = slice.last()?.timestamp;
        let ids = slice.iter().map(|msg| msg.dedup_key()).collect();
        Some(DigestWindow { lo, hi, ids })
    }

    /// The newest `recent` messages as an **open-ended** digest window
    /// (`hi = i64::MAX`): "I hold everything from `lo` onward except the
    /// gaps not in `ids`." This is what drives reconnect recovery — a peer
    /// that froze advertises it, and holders re-send every *newer* message
    /// it lacks (a closed `hi` would never cover messages past the sender's
    /// own newest). `None` only if the log is empty.
    pub(crate) fn recent_window(&self, recent: usize) -> Option<DigestWindow> {
        let start = self.len().saturating_sub(recent);
        let mut window = self.window_at(start, recent)?;
        window.hi = i64::MAX;
        Some(window)
    }

    /// Number of messages older than the newest `recent` — the portion the
    /// rolling [`older_window`](Self::older_window) sweeps.
    pub(crate) fn older_len(&self, recent: usize) -> usize {
        self.len().saturating_sub(recent)
    }

    /// A rolling **closed** window over the older portion (everything before
    /// the newest `recent`): up to `max` ids starting at `start` *within*
    /// that portion, with exact `[lo, hi]` bounds so receivers reconcile
    /// deep interior gaps without re-sending the out-of-window remainder.
    /// `None` when there is no older portion (`len <= recent`).
    pub(crate) fn older_window(
        &self,
        recent: usize,
        start: usize,
        max: usize,
    ) -> Option<DigestWindow> {
        let older_len = self.older_len(recent);
        if older_len == 0 {
            return None;
        }
        let start = start % older_len;
        let count = max.min(older_len - start);
        self.window_at(start, count)
    }

    /// Up to `max` of our messages (newest first) within the `[lo, hi]`
    /// timestamp window whose compact id is **not** in `have` — the
    /// in-window gap to re-broadcast so a peer that advertised that window
    /// recovers what it missed. Out-of-window messages are never re-sent.
    pub(crate) fn missing_in_window(
        &self,
        range: WindowRange,
        have: &HashSet<[u8; 16]>,
        max: usize,
    ) -> Vec<Message> {
        self.messages
            .iter()
            .rev()
            .filter(|msg| {
                msg.timestamp >= range.lo
                    && msg.timestamp <= range.hi
                    && !have.contains(&msg.dedup_key())
            })
            .take(max)
            .cloned()
            .collect()
    }
}

/// Total order deciding which message is evicted on overflow (smallest is
/// dropped): oldest `timestamp` first, then author key, the author's `seq`
/// (so one author's burst evicts in send order — `seq 0` goes first), and
/// finally the message id as a tie-break. Every field travels on the wire
/// and is identical on every node, so retention is a pure function of the
/// message set, not of arrival order.
fn eviction_key(msg: &Message) -> (i64, &str, Option<u64>, &str) {
    (msg.timestamp, msg.pubkey.as_str(), msg.seq, msg.id.as_str())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{Message, MessageLog, WindowRange};

    fn msg(id: &str) -> Message {
        Message::new_app(
            &crate::protocol::MeshId::from("test"),
            &crate::protocol::Nickname::from("author"),
            crate::protocol::AppFrameParams {
                tag: crate::protocol::AppTag::from("app_msg"),
                to: None,
                corr: None,
                body: crate::protocol::MessageBody::from(id),
            },
        )
    }

    /// A message tagged with an explicit timestamp, for window tests.
    fn msg_at(body: &str, ts: i64) -> Message {
        let mut message = msg(body);
        message.timestamp = ts;
        message
    }

    // ── windowed digest ────────────────────────────────────────────

    #[test]
    fn window_at_returns_slice_with_ts_bounds() {
        let mut log = MessageLog::new(10);
        for ts in [10, 20, 30, 40, 50] {
            log.push(msg_at(&ts.to_string(), ts));
        }
        // A middle slice of 2, starting at index 1 (ts=20).
        let window = log.window_at(1, 2).expect("non-empty window");
        assert_eq!((window.lo, window.hi), (20, 30));
        assert_eq!(window.ids.len(), 2);
        // A max wider than the log starting at 0 covers everything.
        let full = log.window_at(0, 100).expect("non-empty window");
        assert_eq!((full.lo, full.hi), (10, 50));
        assert_eq!(full.ids.len(), 5);
        // Past the end ⇒ None.
        assert!(log.window_at(5, 2).is_none());
    }

    #[test]
    fn rolling_window_covers_whole_buffer() {
        // Sweeping the rolling cursor in `max`-sized steps must advertise
        // every id at least once over a full cycle — so a peer behind by
        // more than one window's worth still recovers across rounds.
        let mut log = MessageLog::new(50);
        for index in 0..50 {
            log.push(msg_at(&format!("m{index}"), 100 + index));
        }
        let max = 7;
        let len = log.len();
        let mut advertised: HashSet<[u8; 16]> = HashSet::new();
        let mut cursor = 0usize;
        // ceil(50/7) = 8 rounds covers the cycle; loop a little extra.
        for _ in 0..16 {
            let start = if max >= len { 0 } else { cursor % len };
            let window = log.window_at(start, max).expect("non-empty");
            advertised.extend(window.ids);
            cursor = if max >= len { 0 } else { (start + max) % len };
        }
        assert_eq!(advertised.len(), 50, "every buffered id advertised");
    }

    #[test]
    fn missing_in_window_excludes_out_of_window_and_have() {
        let mut log = MessageLog::new(10);
        for ts in [10, 20, 30, 40, 50] {
            log.push(msg_at(&ts.to_string(), ts));
        }
        // The receiver already has the ts=30 message.
        let have: HashSet<[u8; 16]> = log
            .window_at(2, 1)
            .expect("ts=30 window")
            .ids
            .into_iter()
            .collect();
        let gap = log.missing_in_window(WindowRange { lo: 20, hi: 40 }, &have, 10);
        let bodies: HashSet<&str> = gap.iter().map(|msg| msg.body.as_str()).collect();
        // ts 20 and 40 are in-window and missing; 30 is in `have`; 10 and 50
        // are out of window — never re-sent.
        assert_eq!(bodies, HashSet::from(["20", "40"]));
    }

    #[test]
    fn recent_window_is_open_ended_and_recovers_newer() {
        // The core reconnect-recovery property: a peer that only has the
        // older messages advertises an open-ended newest window, and a
        // holder must offer everything *newer* it lacks. A closed `hi` at
        // the requester's own newest would miss exactly those.
        let mut requester = MessageLog::new(100);
        let mut holder = MessageLog::new(100);
        let mut newer: HashSet<String> = HashSet::new();
        for ts in 1..=10i64 {
            let message = msg_at(&ts.to_string(), ts);
            holder.push(message.clone());
            if ts <= 5 {
                requester.push(message);
            } else {
                newer.insert(ts.to_string());
            }
        }
        let window = requester.recent_window(50).expect("non-empty");
        assert_eq!(window.hi, i64::MAX, "newest window must be open-ended");
        let have: HashSet<[u8; 16]> = window.ids.into_iter().collect();
        let offered: HashSet<String> = holder
            .missing_in_window(
                WindowRange {
                    lo: window.lo,
                    hi: window.hi,
                },
                &have,
                100,
            )
            .iter()
            .map(|msg| msg.body.as_str().to_string())
            .collect();
        assert_eq!(offered, newer, "holder offers exactly the newer messages");
    }

    // ── MessageLog ─────────────────────────────────────────────────

    #[test]
    fn message_log_evicts_lowest_key_when_full() {
        // Eviction drops the smallest eviction key (here: oldest timestamp),
        // not the front, so retention is independent of push order. Push the
        // newest first to prove arrival order doesn't decide who survives.
        let mut log = MessageLog::new(2);
        log.push(msg_at("c", 30));
        log.push(msg_at("a", 10));
        log.push(msg_at("b", 20)); // over cap → evicts "a" (ts=10), the oldest
        assert_eq!(log.messages.len(), 2);
        let bodies: HashSet<&str> = log.messages.iter().map(|msg| msg.body.as_str()).collect();
        assert_eq!(
            bodies,
            HashSet::from(["b", "c"]),
            "kept the two newest by ts"
        );
    }

    mod prop {
        use proptest::{prop_assert, proptest};

        use super::{MessageLog, msg};

        proptest! {
            #![proptest_config(crate::proptest_support::config())]
            #[test]
            fn prop_message_log_never_exceeds_capacity(
                cap in 1..50usize,
                n_pushes in 0..200usize,
            ) {
                let mut log = MessageLog::new(cap);
                for i in 0..n_pushes {
                    log.push(msg(&format!("m{i}")));
                }
                prop_assert!(log.messages.len() <= cap);
            }
        }
    }
}
