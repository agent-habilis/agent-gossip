use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::util::consts::RATE_LIMIT_PER_MIN;
use governor::{
    Quota, RateLimiter,
    clock::DefaultClock,
    middleware::NoOpMiddleware,
    state::{InMemoryState, NotKeyed},
};

use crate::util::tuning::RATE_LIMITER_TTL_SECS;

type Limiter = Arc<RateLimiter<NotKeyed, InMemoryState, DefaultClock, NoOpMiddleware>>;

fn make_limiter(per_min: NonZeroU32) -> Limiter {
    Arc::new(RateLimiter::direct(Quota::per_minute(per_min)))
}

struct LimiterEntry {
    limiter: Limiter,
    last_used: Instant,
}

/// Per-identity rate limiter, keyed by the author's **verified public key**
/// (hex). Keying on the signed pubkey rather than the spoofable nickname
/// means the quota cannot be dodged by switching display names. One quota
/// covers every message — open broadcast or directed reply, no per-kind
/// split. Entries are pruned after `ttl_secs` of inactivity. The cap is the
/// swarm-wide one carried in the id; `per_min == None` means no limit (the
/// id encoded `0`), in which case `check` always admits.
pub(crate) struct SwarmRateLimiter {
    limiters: Mutex<HashMap<String, LimiterEntry>>,
    ttl_secs: u64,
    per_min: Option<NonZeroU32>,
}

impl SwarmRateLimiter {
    pub(crate) fn new() -> Self {
        Self::from_per_min(RATE_LIMIT_PER_MIN)
    }

    /// Build a limiter for `per_min` messages/minute per author. `0`
    /// disables rate limiting entirely (every `check` admits).
    pub(crate) fn from_per_min(per_min: u16) -> Self {
        SwarmRateLimiter {
            limiters: Mutex::new(HashMap::new()),
            ttl_secs: RATE_LIMITER_TTL_SECS,
            per_min: NonZeroU32::new(u32::from(per_min)),
        }
    }

    /// Returns true if a message from the author with public key `key`
    /// (hex) is within the rate limit. Consumes one token on success.
    /// Applied identically on the send and receive paths so a node is held
    /// to the same quota either way. When the swarm disabled rate limiting
    /// (`per_min == 0`), always admits.
    pub(crate) fn check(&self, key: &str) -> bool {
        let Some(per_min) = self.per_min else {
            return true;
        };
        let now = Instant::now();
        let mut map = self.limiters.lock().expect("rate-limiter mutex poisoned");
        // Hot path (key already seen): borrow, no key clone. Only the
        // first sighting needs to own the key.
        if let Some(entry) = map.get_mut(key) {
            entry.last_used = now;
            return entry.limiter.check().is_ok();
        }
        let entry = map.entry(key.to_owned()).or_insert_with(|| LimiterEntry {
            limiter: make_limiter(per_min),
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
            per_min: NonZeroU32::new(u32::from(RATE_LIMIT_PER_MIN)),
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
    use super::{RATE_LIMIT_PER_MIN, SwarmRateLimiter};

    // Keys are author public keys (hex) in production; any distinct string
    // exercises the keying for these tests.
    const ALICE: &str = "alice-pubkey";
    const BOB: &str = "bob-pubkey";

    #[test]
    fn allowed_within_quota() {
        let limiter = SwarmRateLimiter::new();
        for _ in 0..RATE_LIMIT_PER_MIN {
            assert!(limiter.check(ALICE));
        }
    }

    #[test]
    fn rejected_after_quota_exceeded() {
        let limiter = SwarmRateLimiter::new();
        for _ in 0..RATE_LIMIT_PER_MIN {
            limiter.check("spammer-pubkey");
        }
        assert!(!limiter.check("spammer-pubkey"));
    }

    #[test]
    fn limiters_are_per_author() {
        let limiter = SwarmRateLimiter::new();
        for _ in 0..RATE_LIMIT_PER_MIN {
            limiter.check(ALICE);
        }
        assert!(!limiter.check(ALICE));
        assert!(limiter.check(BOB));
    }

    #[test]
    fn default_constructor_works() {
        let limiter = SwarmRateLimiter::default();
        assert!(limiter.check("test-pubkey"));
    }

    #[test]
    fn from_per_min_zero_disables_rate_limiting() {
        let limiter = SwarmRateLimiter::from_per_min(0);
        // Well past any finite quota — all admitted because the swarm
        // encoded `0` (no rate limit).
        for _ in 0..(u32::from(RATE_LIMIT_PER_MIN) * 4) {
            assert!(limiter.check("spammer-pubkey"));
        }
    }

    #[test]
    fn from_per_min_caps_at_the_given_quota() {
        let limiter = SwarmRateLimiter::from_per_min(RATE_LIMIT_PER_MIN);
        for _ in 0..RATE_LIMIT_PER_MIN {
            assert!(limiter.check("author-pubkey"));
        }
        assert!(!limiter.check("author-pubkey"), "over quota is rejected");
    }

    #[test]
    fn prune_removes_expired_entries() {
        let limiter = SwarmRateLimiter::with_ttl(0);
        limiter.check(ALICE);
        limiter.check(BOB);
        assert_eq!(limiter.len(), 2);

        limiter.prune_inactive();
        assert_eq!(limiter.len(), 0);
    }

    #[test]
    fn prune_keeps_recent_entries() {
        let limiter = SwarmRateLimiter::with_ttl(600);
        limiter.check(ALICE);
        limiter.check(BOB);

        limiter.prune_inactive();
        assert_eq!(limiter.len(), 2);
    }

    #[test]
    fn prune_is_selective() {
        let limiter = SwarmRateLimiter::with_ttl(0);
        limiter.check("old-author-pubkey");

        // Immediately prune — "old-author" expires (ttl=0)
        limiter.prune_inactive();
        assert_eq!(limiter.len(), 0);

        // New entry created after prune should survive until next prune
        limiter.check("new-author-pubkey");
        assert_eq!(limiter.len(), 1);
    }

    mod prop {
        use proptest::{prop_assume, proptest, strategy::Strategy};

        use super::{RATE_LIMIT_PER_MIN, SwarmRateLimiter};

        fn arb_key() -> impl Strategy<Value = String> {
            "[a-f0-9]{8,16}".prop_map(|raw| raw)
        }

        proptest! {
            #![proptest_config(crate::proptest_support::config())]
            #[test]
            fn prop_quota_limit_holds(author in arb_key()) {
                let limiter = SwarmRateLimiter::new();
                for _ in 0..RATE_LIMIT_PER_MIN {
                    assert!(limiter.check(&author));
                }
                assert!(!limiter.check(&author));
            }

            #[test]
            fn prop_per_author_isolation(
                author_a in arb_key(),
                author_b in arb_key(),
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
                authors in proptest::collection::vec(arb_key(), 1..20),
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
