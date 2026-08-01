//! Offline microbenchmarks for the pure, deterministic hot paths run on
//! every create/join/broadcast: crypto/identity derivation, the id
//! token + config codec, parsing/validation, and message
//! (de)serialization. No network, no async — divan prints a summary
//! table at the end.
//!
//! Run: `cargo task bench` (or `cargo bench --features bench`). The
//! `bench` feature exposes `agent_gossip::harness::bench`, the in-crate
//! shim over the otherwise-`pub(crate)` internals.

use agent_gossip::harness::bench::{self as api, BenchConfig, BenchMessage};
use agent_gossip::{MeshName, MessageBody, Nickname};
use divan::counter::BytesCount;
use divan::{Bencher, black_box};

fn main() {
    divan::main();
}

const SEED: [u8; 32] = [7u8; 32];
const SHORT_NAME: &str = "ab";
/// 32 chars — the `MeshName` length cap, so this is the encode worst case.
const MAX_NAME: &str = "abcdefghijklmnopqrstuvwxyz012345";

mod crypto {
    use super::{BenchConfig, Bencher, MeshName, SEED, api, black_box};

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
        let name = MeshName::new("bench-team").unwrap();
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
    use super::{BenchConfig, Bencher, MAX_NAME, MeshName, SHORT_NAME, api, black_box};

    fn bench_encode(bencher: Bencher<'_, '_>, raw: &str) {
        let name = MeshName::new(raw).unwrap();
        let config = BenchConfig::public();
        bencher.bench(|| api::mesh_token(black_box(&name), black_box(&config)));
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
        let name = MeshName::new(MAX_NAME).unwrap();
        let token = api::mesh_token(&name, &BenchConfig::public());
        bencher.bench(|| api::mesh_decode(black_box(&token)));
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
    use super::{Bencher, MAX_NAME, MeshName, Nickname, api, black_box};

    // A valid token to exercise the accept path of `MeshId::new`.
    fn valid_token() -> String {
        api::mesh_token(
            &MeshName::new("bench").unwrap(),
            &super::BenchConfig::loopback(),
        )
    }

    #[divan::bench]
    fn mesh_id_valid(bencher: Bencher<'_, '_>) {
        let token = valid_token();
        bencher.bench(|| api::mesh_id_validate(black_box(&token)));
    }

    #[divan::bench]
    fn mesh_id_invalid(bencher: Bencher<'_, '_>) {
        bencher.bench(|| api::mesh_id_validate(black_box("not-an-ahs-id")));
    }

    #[divan::bench]
    fn mesh_name_valid() -> bool {
        MeshName::new(black_box(MAX_NAME)).is_ok()
    }

    #[divan::bench]
    fn mesh_name_invalid() -> bool {
        MeshName::new(black_box("has space / slash")).is_ok()
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

mod validation {
    use super::{MessageBody, black_box};

    // `MessageBody` is public; keep a tiny validation bench.
    #[divan::bench]
    fn message_body_new() -> bool {
        MessageBody::new(black_box("a normal chat line")).is_ok()
    }
}
