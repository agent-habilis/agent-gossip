//! A per-key throttle bounded by construction.
//!
//! Records each key's last `note` time and reports whether it is still within a
//! fixed window. `note` self-prunes expired entries, so — unlike a raw
//! `HashMap<K, Instant>` whose bounded-ness relies on every caller remembering
//! to prune — the map can never outgrow the keys touched within one window. Two
//! instances tame the membership amplifier: the re-link throttle and the
//! `PeerInfo`-reflood throttle (see [`crate::daemon::state::EventLoopState`]).

use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};

/// A set of keys each "on cooldown" until `window` after its last [`note`].
///
/// [`note`]: Cooldown::note
#[derive(Debug)]
pub(crate) struct Cooldown<K> {
    seen: HashMap<K, Instant>,
    window: Duration,
}

impl<K: Eq + Hash + Copy> Cooldown<K> {
    /// A throttle with the given cooldown `window`.
    pub(crate) fn new(window: Duration) -> Self {
        Self {
            seen: HashMap::new(),
            window,
        }
    }

    /// `true` if `key` was [`note`]d within the last `window` of `now`, so the
    /// caller should skip the throttled action.
    ///
    /// [`note`]: Cooldown::note
    pub(crate) fn on_cooldown(&self, key: K, now: Instant) -> bool {
        self.seen
            .get(&key)
            .is_some_and(|at| now.duration_since(*at) < self.window)
    }

    /// Record `key` at `now`, dropping expired entries first so the map stays
    /// bounded by the keys seen within the active window.
    pub(crate) fn note(&mut self, key: K, now: Instant) {
        self.seen
            .retain(|_, at| now.duration_since(*at) < self.window);
        self.seen.insert(key, now);
    }

    /// Live entry count — for the state's bounded-ness assertions.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.seen.len()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::Cooldown;

    const WINDOW: Duration = Duration::from_secs(10);

    #[test]
    fn note_then_on_cooldown_within_window() {
        let mut cooldown: Cooldown<u8> = Cooldown::new(WINDOW);
        let start = Instant::now();
        assert!(
            !cooldown.on_cooldown(1, start),
            "unseen key is never on cooldown"
        );
        cooldown.note(1, start);
        assert!(cooldown.on_cooldown(1, start));
        // Past the window the key is free again (no permanent lockout).
        assert!(!cooldown.on_cooldown(1, start + WINDOW + Duration::from_secs(1)));
    }

    #[test]
    fn is_per_key() {
        let mut cooldown: Cooldown<u8> = Cooldown::new(WINDOW);
        let start = Instant::now();
        cooldown.note(1, start);
        assert!(cooldown.on_cooldown(1, start));
        assert!(
            !cooldown.on_cooldown(2, start),
            "one key's cooldown never gates another"
        );
    }

    #[test]
    fn note_self_prunes_expired_entries() {
        let mut cooldown: Cooldown<u8> = Cooldown::new(WINDOW);
        let start = Instant::now();
        // Many distinct keys spread across more than a full window: each `note`
        // drops entries older than the window, so the map never accumulates all.
        for seed in 0..50u8 {
            cooldown.note(seed, start + Duration::from_secs(u64::from(seed)));
        }
        assert!(
            cooldown.len() <= usize::try_from(WINDOW.as_secs()).unwrap() + 1,
            "expired entries pruned: {}",
            cooldown.len()
        );
    }
}
