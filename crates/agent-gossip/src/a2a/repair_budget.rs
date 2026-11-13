use std::collections::HashMap;
use std::time::{Duration, Instant};

use habilis_network::protocol::AuthorKey;

/// The window of one count.
const WINDOW: Duration = Duration::from_secs(5);

/// The `shard/repair` asks that one member can make in one window.
const PER_MEMBER_MAX: u32 = 8;

/// The `shard/repair` asks that all members can make in one window. The table of members holds at
/// most this many keys, because a key enters the table only with an ask that this cap admits, and
/// the table is cleared with each window.
const GLOBAL_MAX: u32 = 32;

/// How many `shard/repair` asks this node serves. One small ask makes this node send the cached
/// frames of a whole group again, so a member must not be able to take the whole service.
///
/// A refused ask is not served, and the member asks again on its next tick.
pub(crate) struct RepairBudget {
    window_start: Instant,
    total: u32,
    per_member: HashMap<AuthorKey, u32>,
}

impl RepairBudget {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            window_start: now,
            total: 0,
            per_member: HashMap::new(),
        }
    }

    /// Whether the ask of `member` at `now` is served. An ask that is served counts.
    pub(crate) fn admit(&mut self, member: AuthorKey, now: Instant) -> bool {
        if now.saturating_duration_since(self.window_start) >= WINDOW {
            self.window_start = now;
            self.total = 0;
            self.per_member.clear();
        }
        if self.total >= GLOBAL_MAX {
            return false;
        }
        let asked = self.per_member.entry(member).or_insert(0);
        if *asked >= PER_MEMBER_MAX {
            return false;
        }
        *asked += 1;
        self.total += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use habilis_network::protocol::AuthorKey;

    use super::{GLOBAL_MAX, PER_MEMBER_MAX, RepairBudget, WINDOW};

    fn member(byte: u8) -> AuthorKey {
        AuthorKey::from([byte; 32])
    }

    #[test]
    fn one_member_gets_its_share_and_no_more_in_one_window() {
        let now = Instant::now();
        let mut budget = RepairBudget::new(now);
        let served = (0..PER_MEMBER_MAX * 10)
            .filter(|_| budget.admit(member(1), now))
            .count();
        assert_eq!(
            served, PER_MEMBER_MAX as usize,
            "10 times the share of one member serves exactly the share"
        );
    }

    #[test]
    fn all_members_together_get_the_global_cap_and_no_more() {
        let now = Instant::now();
        let mut budget = RepairBudget::new(now);
        let asks = (0..=255u8).flat_map(|byte| (0..PER_MEMBER_MAX).map(move |_| byte));
        let served = asks.filter(|byte| budget.admit(member(*byte), now)).count();
        assert_eq!(
            served, GLOBAL_MAX as usize,
            "256 members that ask at the cap are served the global cap"
        );
        assert!(
            budget.per_member.len() <= GLOBAL_MAX as usize,
            "the table of members never holds more keys than the global cap"
        );
    }

    #[test]
    fn a_new_window_serves_a_member_again_and_clears_the_table() {
        let now = Instant::now();
        let mut budget = RepairBudget::new(now);
        for _ in 0..PER_MEMBER_MAX {
            assert!(budget.admit(member(1), now), "the share is served");
        }
        assert!(!budget.admit(member(1), now), "the next ask is refused");
        let later = now + WINDOW + Duration::from_millis(1);
        assert!(
            budget.admit(member(1), later),
            "the member is served again in the next window"
        );
        assert_eq!(
            budget.per_member.len(),
            1,
            "the table holds only the member of the new window"
        );
    }
}
