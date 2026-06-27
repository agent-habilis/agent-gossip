//! Offline microbenchmarks for the pure, deterministic hot paths run on
//! every create/join/broadcast: crypto/identity derivation, the `ahsw…`
//! token + config codec, parsing/validation, message (de)serialization,
//! and the rate limiter. No network, no async — divan prints a summary
//! table at the end.
//!
//! Run: `cargo task bench` (or `cargo bench --features bench`). The
//! `bench` feature exposes `agent_habilis_swarm::harness::bench`, the in-crate
//! shim over the otherwise-`pub(crate)` internals.

use agent_habilis_swarm::harness::bench::{
    self as api, BenchConfig, BenchMessage, BenchRateLimiter,
};
use agent_habilis_swarm::{MessageBody, Nickname, SwarmName};
use divan::counter::{BytesCount, ItemsCount};
use divan::{Bencher, black_box};

fn main() {
    divan::main();
}

const SEED: [u8; 32] = [7u8; 32];
const SHORT_NAME: &str = "ab";
/// 32 chars — the `SwarmName` length cap, so this is the encode worst case.
const MAX_NAME: &str = "abcdefghijklmnopqrstuvwxyz012345";

mod crypto {
    use super::{BenchConfig, Bencher, SEED, SwarmName, api, black_box};

    #[divan::bench]
    fn derive_secret() -> [u8; 32] {
        api::derive_secret(black_box(&SEED), black_box(b"rendezvous"))
    }

    #[divan::bench]
    fn rendezvous_ports() -> [u16; 8] {
        api::rendezvous_ports(black_box(&SEED))
    }

    #[divan::bench]
    fn rendezvous_id() {
        api::rendezvous_id(black_box(&SEED));
    }

    fn bench_topic(bencher: Bencher<'_, '_>, config: &BenchConfig) {
        let name = SwarmName::new("bench-team").unwrap();
        bencher
            .bench(|| api::derive_topic_id(black_box(&SEED), black_box(&name), black_box(config)));
    }

    #[divan::bench]
    fn derive_topic_id_loopback(bencher: Bencher<'_, '_>) {
        bench_topic(bencher, &BenchConfig::loopback());
    }

    #[divan::bench]
    fn derive_topic_id_public(bencher: Bencher<'_, '_>) {
        bench_topic(bencher, &BenchConfig::public());
    }
}

mod token {
    use super::{BenchConfig, Bencher, MAX_NAME, SHORT_NAME, SwarmName, api, black_box};

    fn bench_encode(bencher: Bencher<'_, '_>, raw: &str) {
        let name = SwarmName::new(raw).unwrap();
        let config = BenchConfig::public();
        bencher.bench(|| api::swarm_token(black_box(&name), black_box(&config)));
    }

    #[divan::bench]
    fn encode_short_name(bencher: Bencher<'_, '_>) {
        bench_encode(bencher, SHORT_NAME);
    }

    #[divan::bench]
    fn encode_max_name(bencher: Bencher<'_, '_>) {
        bench_encode(bencher, MAX_NAME);
    }

    #[divan::bench]
    fn decode(bencher: Bencher<'_, '_>) {
        let name = SwarmName::new(MAX_NAME).unwrap();
        let token = api::swarm_token(&name, &BenchConfig::public());
        bencher.bench(|| api::swarm_decode(black_box(&token)));
    }

    #[divan::bench]
    fn config_round_trip_loopback(bencher: Bencher<'_, '_>) {
        let config = BenchConfig::loopback();
        bencher.bench(|| api::config_round_trip(black_box(&config)));
    }

    #[divan::bench]
    fn config_round_trip_custom_relay(bencher: Bencher<'_, '_>) {
        let config = BenchConfig::custom_relay();
        bencher.bench(|| api::config_round_trip(black_box(&config)));
    }
}

mod parsing {
    use super::{Bencher, MAX_NAME, Nickname, SwarmName, api, black_box};

    // A valid `ahsw…` token to exercise the accept path of `SwarmId::new`.
    fn valid_token() -> String {
        api::swarm_token(
            &SwarmName::new("bench").unwrap(),
            &super::BenchConfig::loopback(),
        )
    }

    #[divan::bench]
    fn swarm_id_valid(bencher: Bencher<'_, '_>) {
        let token = valid_token();
        bencher.bench(|| api::swarm_id_validate(black_box(&token)));
    }

    #[divan::bench]
    fn swarm_id_invalid(bencher: Bencher<'_, '_>) {
        bencher.bench(|| api::swarm_id_validate(black_box("not-an-ahs-id")));
    }

    #[divan::bench]
    fn swarm_name_valid() -> bool {
        SwarmName::new(black_box(MAX_NAME)).is_ok()
    }

    #[divan::bench]
    fn swarm_name_invalid() -> bool {
        SwarmName::new(black_box("has space / slash")).is_ok()
    }

    #[divan::bench]
    fn nickname_valid() -> bool {
        Nickname::new(black_box("swift-cedar")).is_ok()
    }

    #[divan::bench]
    fn nickname_invalid() -> bool {
        Nickname::new(black_box("bad#nick")).is_ok()
    }
}

mod message {
    use super::{BenchMessage, Bencher, BytesCount, api, black_box};

    const BODY: &str = "the quick brown fox jumps over the lazy dog, repeated a few times \
                        to land a realistic chat-message body length for the wire path.";

    #[divan::bench]
    fn serialize(bencher: Bencher<'_, '_>) {
        let msg = BenchMessage::sample(BODY);
        bencher
            .counter(BytesCount::new(BODY.len()))
            .bench(|| msg.serialize());
    }

    #[divan::bench]
    fn deserialize(bencher: Bencher<'_, '_>) {
        let bytes = BenchMessage::sample(BODY).serialize();
        bencher
            .counter(BytesCount::new(bytes.len()))
            .bench(|| api::message_deserialize(black_box(&bytes)));
    }
}

mod rate_limit {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{BenchRateLimiter, Bencher, ItemsCount, MessageBody, black_box};

    /// Far above the production cap (60/min) so a bench run never exhausts
    /// the bucket mid-measurement — we time the per-call cost, not the
    /// rejection path.
    const HIGH_QUOTA: u16 = 60_000;

    #[divan::bench]
    fn check_known_author(bencher: Bencher<'_, '_>) {
        let limiter = BenchRateLimiter::new(HIGH_QUOTA);
        let author = "steady-author-pubkey";
        // Prime the map so every timed call takes the borrow path.
        limiter.check(author);
        bencher
            .counter(ItemsCount::new(1usize))
            .bench(|| black_box(limiter.check(black_box(author))));
    }

    // First-sighting path: a never-seen author each iteration drives the
    // `entry()` insert + governor construction. `with_inputs` builds the
    // fresh nickname *untimed*, so the measurement is the limiter cost
    // alone, not `format!`/`Nickname::new`.
    #[divan::bench]
    fn check_cold_author(bencher: Bencher<'_, '_>) {
        let limiter = BenchRateLimiter::new(HIGH_QUOTA);
        // Atomic so the input generator stays `Fn` (divan requires it)
        // while still yielding a fresh, never-seen author per iteration.
        let counter = AtomicU64::new(0);
        bencher
            .counter(ItemsCount::new(1usize))
            .with_inputs(|| {
                let next = counter.fetch_add(1, Ordering::Relaxed);
                format!("a{next}")
            })
            .bench_refs(|author| black_box(limiter.check(author)));
    }

    // `rate_limit_per_min == 0` → the unlimited early-return (no lock, no
    // map) — should be ~free.
    #[divan::bench]
    fn check_unlimited(bencher: Bencher<'_, '_>) {
        let limiter = BenchRateLimiter::new(0);
        let author = "steady-author-pubkey";
        bencher
            .counter(ItemsCount::new(1usize))
            .bench(|| black_box(limiter.check(black_box(author))));
    }

    // `MessageBody` is public; keep a tiny validation bench alongside.
    #[divan::bench]
    fn message_body_new() -> bool {
        MessageBody::new(black_box("a normal chat line")).is_ok()
    }
}
