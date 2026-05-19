use std::collections::{HashSet, VecDeque};

use crate::protocol::{Message, MessageId};

/// A bounded FIFO buffer storing all message types for the poll command.
/// When full, the oldest message is discarded.
pub(crate) struct MessageLog {
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

    /// Add a message to the log. If full, removes the oldest.
    pub(crate) fn push(&mut self, msg: Message) {
        if self.messages.len() >= self.capacity {
            self.messages.pop_front();
        }
        self.messages.push_back(msg);
    }

    /// The `max` most-recent message ids (newest first) — the payload
    /// of an anti-entropy digest advertising what we hold.
    pub(crate) fn recent_ids(&self, max: usize) -> Vec<&str> {
        self.messages
            .iter()
            .rev()
            .take(max)
            .map(|msg| msg.id.as_str())
            .collect()
    }

    /// Up to `max` of our messages (newest first) whose id is **not**
    /// in `have` — the gap to re-broadcast so a peer that sent us that
    /// digest recovers what it missed.
    pub(crate) fn missing_from(&self, have: &HashSet<&str>, max: usize) -> Vec<Message> {
        self.messages
            .iter()
            .rev()
            .filter(|msg| !have.contains(msg.id.as_str()))
            .take(max)
            .cloned()
            .collect()
    }

    /// Return messages after the given ID, or all messages if `after` is None.
    /// Returns (messages, evicted) where evicted is true if the requested ID
    /// was not found in the buffer (likely evicted due to capacity).
    pub(crate) fn messages_after(&self, after: Option<&MessageId>) -> (Vec<Message>, bool) {
        match after {
            None => (self.messages.iter().cloned().collect(), false),
            Some(id) => {
                let pos = self.messages.iter().position(|msg| &msg.id == id);
                match pos {
                    Some(idx) => (self.messages.iter().skip(idx + 1).cloned().collect(), false),
                    None => (self.messages.iter().cloned().collect(), true),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str) -> Message {
        Message::new_message(
            &crate::protocol::SwarmId::from("ahstest"),
            &crate::protocol::Nickname::from("author"),
            crate::protocol::MessageBody::from(id),
        )
    }

    // ── MessageLog ─────────────────────────────────────────────────

    #[test]
    fn message_log_returns_all_when_no_after() {
        let mut log = MessageLog::new(10);
        let m1 = msg("first");
        let m2 = msg("second");
        log.push(m1);
        log.push(m2);
        let (msgs, evicted) = log.messages_after(None);
        assert_eq!(msgs.len(), 2);
        assert!(!evicted);
    }

    #[test]
    fn message_log_returns_after_id() {
        let mut log = MessageLog::new(10);
        let m1 = msg("first");
        let id1 = m1.id.clone();
        let m2 = msg("second");
        let m3 = msg("third");
        log.push(m1);
        log.push(m2);
        log.push(m3);
        let (msgs, evicted) = log.messages_after(Some(&id1));
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].body.as_str(), "second");
        assert_eq!(msgs[1].body.as_str(), "third");
        assert!(!evicted);
    }

    #[test]
    fn message_log_returns_all_with_evicted_flag_when_id_not_found() {
        let mut log = MessageLog::new(2);
        let m1 = msg("first");
        let id1 = m1.id.clone();
        log.push(m1);
        log.push(msg("second"));
        log.push(msg("third")); // evicts "first"
        let (msgs, evicted) = log.messages_after(Some(&id1));
        assert!(evicted);
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn message_log_after_last_returns_empty() {
        let mut log = MessageLog::new(10);
        let stored = msg("only");
        let id = stored.id.clone();
        log.push(stored);
        let (msgs, evicted) = log.messages_after(Some(&id));
        assert!(msgs.is_empty());
        assert!(!evicted);
    }

    #[test]
    fn message_log_evicts_oldest_when_full() {
        let mut log = MessageLog::new(2);
        log.push(msg("a"));
        log.push(msg("b"));
        log.push(msg("c"));
        assert_eq!(log.messages.len(), 2);
        assert_eq!(log.messages[0].body.as_str(), "b");
        assert_eq!(log.messages[1].body.as_str(), "c");
    }

    mod prop {
        use super::*;
        use proptest::prelude::*;

        proptest! {
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

            #[test]
            fn prop_messages_after_none_returns_all(
                count in 0..50usize,
            ) {
                let mut log = MessageLog::new(100);
                for i in 0..count {
                    log.push(msg(&format!("m{i}")));
                }
                let (msgs, evicted) = log.messages_after(None);
                prop_assert_eq!(msgs.len(), count);
                prop_assert!(!evicted);
            }

            #[test]
            fn prop_messages_after_valid_id_excludes_prefix(
                before in 1..20usize,
                after in 0..20usize,
            ) {
                let mut log = MessageLog::new(100);
                let mut ids = Vec::new();
                for i in 0..(before + after) {
                    let stored = msg(&format!("m{i}"));
                    ids.push(stored.id.clone());
                    log.push(stored);
                }
                // Query after the `before`-th message (0-indexed: before - 1)
                let pivot = &ids[before - 1];
                let (msgs, evicted) = log.messages_after(Some(pivot));
                prop_assert_eq!(msgs.len(), after);
                prop_assert!(!evicted);
            }

            #[test]
            fn prop_evicted_id_returns_all_with_flag(
                cap in 1..10usize,
                extra in 1..20usize,
            ) {
                let mut log = MessageLog::new(cap);
                let first = msg("evicted");
                let evicted_id = first.id.clone();
                log.push(first);
                // Push enough to evict
                for i in 0..(cap + extra) {
                    log.push(msg(&format!("fill{i}")));
                }
                let (msgs, evicted) = log.messages_after(Some(&evicted_id));
                prop_assert!(evicted);
                prop_assert_eq!(msgs.len(), log.messages.len());
            }
        }
    }
}
