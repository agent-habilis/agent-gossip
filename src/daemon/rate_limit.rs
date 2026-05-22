use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ahs_shared::RATE_LIMIT_PER_MIN;
use governor::{
    Quota, RateLimiter,
    clock::DefaultClock,
    middleware::NoOpMiddleware,
    state::{InMemoryState, NotKeyed},
};

use crate::protocol::Nickname;
use crate::util::tuning::RATE_LIMITER_TTL_SECS;

type Limiter = Arc<RateLimiter<NotKeyed, InMemoryState, DefaultClock, NoOpMiddleware>>;

fn make_limiter() -> Limiter {
    let quota = Quota::per_minute(NonZeroU32::new(RATE_LIMIT_PER_MIN).unwrap());
    Arc::new(RateLimiter::direct(quota))
}

struct LimiterEntry {
    limiter: Limiter,
    last_used: Instant,
}

/// Per-identity rate limiter, keyed by author nickname. One quota covers
/// every message — open broadcast or directed reply, no per-kind split.
/// Entries are pruned after `ttl_secs` of inactivity.
pub(crate) struct SwarmRateLimiter {
    limiters: Mutex<HashMap<Nickname, LimiterEntry>>,
    ttl_secs: u64,
}

impl SwarmRateLimiter {
    pub(crate) fn new() -> Self {
        SwarmRateLimiter {
            limiters: Mutex::new(HashMap::new()),
            ttl_secs: RATE_LIMITER_TTL_SECS,
        }
    }

    /// Returns true if a message from `author` is within the rate limit.
    /// Consumes one token on success. Applied identically on the send and
    /// receive paths so a node is held to the same quota either way.
    pub(crate) fn check(&self, author: &Nickname) -> bool {
        let now = Instant::now();
        let mut map = self.limiters.lock().expect("rate-limiter mutex poisoned");
        // Hot path (author already seen): borrow, no key clone. Only the
        // first sighting needs to own the key.
        if let Some(entry) = map.get_mut(author) {
            entry.last_used = now;
            return entry.limiter.check().is_ok();
        }
        let entry = map.entry(author.clone()).or_insert_with(|| LimiterEntry {
            limiter: make_limiter(),
            last_used: now,
        });
        entry.limiter.check().is_ok()
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
    fn allowed_within_quota() {
        let limiter = SwarmRateLimiter::new();
        let alice = nick("alice");
        for _ in 0..RATE_LIMIT_PER_MIN {
            assert!(limiter.check(&alice));
        }
    }

    #[test]
    fn rejected_after_quota_exceeded() {
        let limiter = SwarmRateLimiter::new();
        let spammer = nick("spammer");
        for _ in 0..RATE_LIMIT_PER_MIN {
            limiter.check(&spammer);
        }
        assert!(!limiter.check(&spammer));
    }

    #[test]
    fn limiters_are_per_author() {
        let limiter = SwarmRateLimiter::new();
        let alice = nick("alice");
        let bob = nick("bob");
        for _ in 0..RATE_LIMIT_PER_MIN {
            limiter.check(&alice);
        }
        assert!(!limiter.check(&alice));
        assert!(limiter.check(&bob));
    }

    #[test]
    fn default_constructor_works() {
        let limiter = SwarmRateLimiter::default();
        assert!(limiter.check(&nick("test")));
    }

    #[test]
    fn prune_removes_expired_entries() {
        let limiter = SwarmRateLimiter::with_ttl(0);
        limiter.check(&nick("alice"));
        limiter.check(&nick("bob"));
        assert_eq!(limiter.len(), 2);

        limiter.prune_inactive();
        assert_eq!(limiter.len(), 0);
    }

    #[test]
    fn prune_keeps_recent_entries() {
        let limiter = SwarmRateLimiter::with_ttl(600);
        limiter.check(&nick("alice"));
        limiter.check(&nick("bob"));

        limiter.prune_inactive();
        assert_eq!(limiter.len(), 2);
    }

    #[test]
    fn prune_is_selective() {
        let limiter = SwarmRateLimiter::with_ttl(0);
        limiter.check(&nick("old-author"));

        // Immediately prune — "old-author" expires (ttl=0)
        limiter.prune_inactive();
        assert_eq!(limiter.len(), 0);

        // New entry created after prune should survive until next prune
        limiter.check(&nick("new-author"));
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
            fn prop_quota_limit_holds(author in arb_nickname()) {
                let limiter = SwarmRateLimiter::new();
                for _ in 0..RATE_LIMIT_PER_MIN {
                    assert!(limiter.check(&author));
                }
                assert!(!limiter.check(&author));
            }

            #[test]
            fn prop_per_author_isolation(
                author_a in arb_nickname(),
                author_b in arb_nickname(),
            ) {
                prop_assume!(author_a != author_b);
                let limiter = SwarmRateLimiter::new();
                // Exhaust author_a's quota.
                for _ in 0..RATE_LIMIT_PER_MIN {
                    limiter.check(&author_a);
                }
                assert!(!limiter.check(&author_a));
                // author_b must still have its full quota available.
                for _ in 0..RATE_LIMIT_PER_MIN {
                    assert!(limiter.check(&author_b));
                }
            }

            #[test]
            fn prop_prune_never_exceeds_input_count(
                authors in proptest::collection::vec(arb_nickname(), 1..20),
            ) {
                let limiter = SwarmRateLimiter::with_ttl(600);
                for author in &authors {
                    limiter.check(author);
                }
                limiter.prune_inactive();
                let unique: std::collections::HashSet<_> = authors.iter().collect();
                assert!(limiter.len() <= unique.len());
            }
        }
    }
}
