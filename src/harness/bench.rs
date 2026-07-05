//! Not public API. Exposed only under the `bench` feature so the divan
//! microbenchmark suite (`benches/hot_paths.rs`) can measure `pub(crate)`
//! hot paths without widening the crate's curated public surface.
//!
//! Only **non-test** constructors are used here: the `bench` feature is
//! not `cfg(test)`, so `#[cfg(test)]` helpers (`Nickname::from`,
//! `SwarmConfig::loopback()`) are unavailable — we build the equivalents
//! from the public/`pub(crate)` non-test items instead.
#![allow(missing_docs, reason = "internal bench shim, doc-hidden")]
#![allow(
    missing_debug_implementations,
    reason = "opaque bench-only newtypes; never surfaced or formatted"
)]

use crate::protocol::crypto;
use crate::protocol::swarm::{LookupOpts, RelayChoice, Swarm, SwarmConfig};
use crate::{Message, MessageBody, Nickname, SwarmId, SwarmName};

/// A swarm config built from non-test constructors (the `SwarmConfig`
/// ctors are `#[cfg(test)]`). `loopback` = no lookups; `public` = the
/// all-on preset; `custom_relay` exercises the relay-URL codec.
pub struct BenchConfig(SwarmConfig);

impl BenchConfig {
    #[must_use]
    pub fn loopback() -> Self {
        Self(cfg(LookupOpts::loopback()))
    }

    #[must_use]
    pub fn public() -> Self {
        Self(cfg(LookupOpts::public_preset()))
    }

    /// Public preset with a 2-rung custom relay ladder, so the encode/
    /// decode path walks the variable-length relay-URL region.
    #[must_use]
    pub fn custom_relay() -> Self {
        let lookups = LookupOpts {
            mdns: true,
            dht: false,
            relay: RelayChoice::Custom(vec![
                "https://a.example".parse().expect("valid relay url"),
                "https://b.example".parse().expect("valid relay url"),
            ]),
        };
        Self(cfg(lookups))
    }
}

fn cfg(lookups: LookupOpts) -> SwarmConfig {
    SwarmConfig {
        lookups,
        password: None,
        issuer_pubkey: None,
    }
}

// ── crypto / identity ───────────────────────────────────────────────

#[must_use]
pub fn derive_secret(seed: &[u8; 32], label: &[u8]) -> [u8; 32] {
    crypto::derive_secret(seed, label)
}

#[must_use]
pub fn rendezvous_ports(seed: &[u8; 32]) -> [u16; crypto::RENDEZVOUS_LADDER] {
    crypto::rendezvous_ports(seed)
}

/// Time the rendezvous-id derivation; the `EndpointId` is iroh-typed, so
/// drop it rather than surface it across the bench boundary.
pub fn rendezvous_id(seed: &[u8; 32]) {
    let _ = crypto::rendezvous_id(seed);
}

pub fn derive_topic_id(seed: &[u8; 32], name: &SwarmName, config: &BenchConfig) {
    let _ = crypto::derive_topic_id(seed, name, &config.0.to_bytes());
}

// ── swarm id (token) round-trip — config is part of the id ──────────

#[must_use]
pub fn swarm_token(name: &SwarmName, config: &BenchConfig) -> String {
    Swarm::new([7u8; 32], name.clone(), config.0.clone()).to_string()
}

pub fn swarm_decode(token: &str) {
    let _ = token.parse::<Swarm>();
}

/// Encode then decode the config region — exercises the lookup-flags +
/// relay-URL byte codec in isolation.
pub fn config_round_trip(config: &BenchConfig) {
    let _ = SwarmConfig::from_bytes(&config.0.to_bytes());
}

// ── parsing / validation ────────────────────────────────────────────
// `SwarmName::new` / `Nickname::new` / `MessageBody::new` are public and
// benched directly; only `SwarmId::new` is `pub(crate)`.

#[must_use]
pub fn swarm_id_validate(value: &str) -> bool {
    SwarmId::new(value).is_ok()
}

// ── message serialize / deserialize ─────────────────────────────────

pub struct BenchMessage(Message);

impl BenchMessage {
    #[must_use]
    pub fn sample(body: &str) -> Self {
        let name = SwarmName::new("bench").expect("valid swarm name");
        let swarm = SwarmId::new(swarm_token(&name, &BenchConfig::loopback()))
            .expect("Swarm::to_string is a valid SwarmId");
        let author = Nickname::new("bench-author").expect("valid nickname");
        Self(Message::new_a2a_msg(
            &swarm,
            &author,
            MessageBody::new(body).expect("valid body"),
        ))
    }

    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        self.0
            .serialize()
            .expect("message under size cap serializes")
    }
}

pub fn message_deserialize(bytes: &[u8]) {
    let _: Message = serde_json::from_slice(bytes).expect("round-trips its own serialize");
}
