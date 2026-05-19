use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use governor::{
    Quota, RateLimiter,
    clock::DefaultClock,
    middleware::NoOpMiddleware,
    state::{InMemoryState, NotKeyed},
};

use crate::protocol::Nickname;
use crate::util::tuning::{
    RATE_LIMIT_MESSAGES_BURST, RATE_LIMIT_MESSAGES_PER_MIN, RATE_LIMIT_REPLIES_BURST,
    RATE_LIMIT_REPLIES_PER_MIN, RATE_LIMITER_TTL_SECS,
};

type Limiter = Arc<RateLimiter<NotKeyed, InMemoryState, DefaultClock, NoOpMiddleware>>;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum MsgKind {
    Message,
    Reply,
}

fn make_limiter(kind: MsgKind) -> Limiter {
    let (per_min, burst) = match kind {
        MsgKind::Message => (RATE_LIMIT_MESSAGES_PER_MIN, RATE_LIMIT_MESSAGES_BURST),
        MsgKind::Reply => (RATE_LIMIT_REPLIES_PER_MIN, RATE_LIMIT_REPLIES_BURST),
    };
    let quota = Quota::per_minute(NonZeroU32::new(per_min).unwrap())
        .allow_burst(NonZeroU32::new(burst).unwrap());
    Arc::new(RateLimiter::direct(quota))
}

struct LimiterEntry {
    limiter: Limiter,
    last_used: Instant,
}

/// Per-identity rate limiters, keyed by (author, message kind).
/// Entries are pruned after `ttl_secs` of inactivity.
pub(crate) struct SwarmRateLimiter {
    limiters: Mutex<HashMap<(Nickname, MsgKind), LimiterEntry>>,
    ttl_secs: u64,
}

impl SwarmRateLimiter {
    pub(crate) fn new() -> Self {
        SwarmRateLimiter {
            limiters: Mutex::new(HashMap::new()),
            ttl_secs: RATE_LIMITER_TTL_SECS,
        }
    }

    fn check(&self, author: &Nickname, kind: MsgKind) -> bool {
        let now = Instant::now();
        let mut map = self.limiters.lock().expect("rate-limiter mutex poisoned");
        let entry = map
            .entry((author.clone(), kind))
            .or_insert_with(|| LimiterEntry {
                limiter: make_limiter(kind),
                last_used: now,
            });
        entry.last_used = now;
        entry.limiter.check().is_ok()
    }

    /// Returns true if the open message from `author` is allowed.
    pub(crate) fn check_message(&self, author: &Nickname) -> bool {
        self.check(author, MsgKind::Message)
    }

    /// Returns true if the directed reply from `author` is allowed.
    pub(crate) fn check_reply(&self, author: &Nickname) -> bool {
        self.check(author, MsgKind::Reply)
    }

    pub(crate) fn prune_inactive(&self) {
        let ttl = self.ttl_secs;
        let mut map = self.limiters.lock().expect("rate-limiter mutex poisoned");
        map.retain(|_, entry| entry.last_used.elapsed().as_secs() < ttl);
    }
}

impl Default for SwarmRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl SwarmRateLimiter {
    fn with_ttl(ttl_secs: u64) -> Self {
        SwarmRateLimiter {
            limiters: Mutex::new(HashMap::new()),
            ttl_secs,
        }
    }

    fn len(&self) -> usize {
        self.limiters
            .lock()
            .expect("rate-limiter mutex poisoned")
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nick(text: &str) -> Nickname {
        Nickname::from(text)
    }

    #[test]
    fn message_allowed_within_burst() {
        let limiter = SwarmRateLimiter::new();
        let alice = nick("alice");
        for _ in 0..5 {
            assert!(limiter.check_message(&alice));
        }
    }

    #[test]
    fn message_rejected_after_burst_exceeded() {
        let limiter = SwarmRateLimiter::new();
        let spammer = nick("spammer");
        for _ in 0..15 {
            limiter.check_message(&spammer);
        }
        assert!(!limiter.check_message(&spammer));
    }

    #[test]
    fn reply_allowed_within_burst() {
        let limiter = SwarmRateLimiter::new();
        let bob = nick("bob");
        for _ in 0..10 {
            assert!(limiter.check_reply(&bob));
        }
    }

    #[test]
    fn reply_rejected_after_burst_exceeded() {
        let limiter = SwarmRateLimiter::new();
        let spammer = nick("spammer");
        for _ in 0..60 {
            limiter.check_reply(&spammer);
        }
        assert!(!limiter.check_reply(&spammer));
    }

    #[test]
    fn limiters_are_per_author() {
        let limiter = SwarmRateLimiter::new();
        let alice = nick("alice");
        let bob = nick("bob");
        for _ in 0..15 {
            limiter.check_message(&alice);
        }
        assert!(!limiter.check_message(&alice));
        assert!(limiter.check_message(&bob));
    }

    #[test]
    fn default_constructor_works() {
        let limiter = SwarmRateLimiter::default();
        assert!(limiter.check_message(&nick("test")));
    }

    #[test]
    fn prune_removes_expired_entries() {
        let limiter = SwarmRateLimiter::with_ttl(0);
        limiter.check_message(&nick("alice"));
        limiter.check_reply(&nick("bob"));
        assert_eq!(limiter.len(), 2);

        limiter.prune_inactive();
        assert_eq!(limiter.len(), 0);
    }

    #[test]
    fn prune_keeps_recent_entries() {
        let limiter = SwarmRateLimiter::with_ttl(600);
        limiter.check_message(&nick("alice"));
        limiter.check_reply(&nick("bob"));

        limiter.prune_inactive();
        assert_eq!(limiter.len(), 2);
    }

    #[test]
    fn prune_is_selective() {
        let limiter = SwarmRateLimiter::with_ttl(0);
        limiter.check_message(&nick("old-author"));

        // Immediately prune — "old-author" expires (ttl=0)
        limiter.prune_inactive();
        assert_eq!(limiter.len(), 0);

        // New entry created after prune should survive until next prune
        limiter.check_message(&nick("new-author"));
        assert_eq!(limiter.len(), 1);
    }

    mod prop {
        use super::*;
        use proptest::prelude::*;

        fn arb_nickname() -> impl Strategy<Value = Nickname> {
            "[a-z]{3,8}-[a-z]{3,8}".prop_map(|raw| Nickname::new(raw).unwrap())
        }

        proptest! {
            #[test]
            fn prop_burst_limit_holds_for_messages(author in arb_nickname()) {
                let limiter = SwarmRateLimiter::new();
                for _ in 0..RATE_LIMIT_MESSAGES_BURST {
                    assert!(limiter.check_message(&author));
                }
                assert!(!limiter.check_message(&author));
            }

            #[test]
            fn prop_burst_limit_holds_for_replies(author in arb_nickname()) {
                let limiter = SwarmRateLimiter::new();
                for _ in 0..RATE_LIMIT_REPLIES_BURST {
                    assert!(limiter.check_reply(&author));
                }
                assert!(!limiter.check_reply(&author));
            }

            #[test]
            fn prop_per_author_isolation(
                author_a in arb_nickname(),
                author_b in arb_nickname(),
            ) {
                prop_assume!(author_a != author_b);
                let limiter = SwarmRateLimiter::new();
                // Exhaust author_a's message burst
                for _ in 0..RATE_LIMIT_MESSAGES_BURST {
                    limiter.check_message(&author_a);
                }
                assert!(!limiter.check_message(&author_a));
                // author_b must still have full burst available
                for _ in 0..RATE_LIMIT_MESSAGES_BURST {
                    assert!(limiter.check_message(&author_b));
                }
            }

            #[test]
            fn prop_message_reply_independence(author in arb_nickname()) {
                let limiter = SwarmRateLimiter::new();
                // Exhaust message burst
                for _ in 0..RATE_LIMIT_MESSAGES_BURST {
                    limiter.check_message(&author);
                }
                assert!(!limiter.check_message(&author));
                // Reply burst must still be fully available
                for _ in 0..RATE_LIMIT_REPLIES_BURST {
                    assert!(limiter.check_reply(&author));
                }
            }

            #[test]
            fn prop_prune_never_exceeds_input_count(
                authors in proptest::collection::vec(arb_nickname(), 1..20),
            ) {
                let limiter = SwarmRateLimiter::with_ttl(600);
                for author in &authors {
                    limiter.check_message(author);
                }
                limiter.prune_inactive();
                let unique: std::collections::HashSet<_> = authors.iter().collect();
                assert!(limiter.len() <= unique.len());
            }
        }
    }
}
