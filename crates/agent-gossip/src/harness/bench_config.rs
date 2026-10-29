//! Not public API. Exposed only under the `bench` feature so the divan
//! microbenchmark suite (`benches/hot_paths.rs`) can measure `pub(crate)`
//! hot paths without widening the crate's curated public surface.
//!
//! Only **non-test** constructors are used here: the `bench` feature is
//! not `cfg(test)`, so `#[cfg(test)]` helpers (`Nickname::from`,
//! `MeshConfig::loopback()`) are unavailable — we build the equivalents
//! from the public/`pub(crate)` non-test items instead.

use habilis_network::iroh::RelayUrl;
use habilis_network::protocol::{
    Identity, LockedMesh, LookupOpts, Mesh, MeshConfig, RelayChoice, Seed, Verified,
};

use crate::a2a::wire::{BROADCAST, app_tag};
use crate::{MeshId, MeshName, Message, MessageBody, Nickname, policy};

/// A mesh config built from non-test constructors (the `MeshConfig`
/// ctors are `#[cfg(test)]`). `loopback` = no lookups; `public` = the
/// all-on preset; `custom_relay` exercises the relay-URL codec.
#[derive(Debug)]
pub struct BenchConfig(MeshConfig);

impl BenchConfig {
    /// A config with every lookup off, so no bench touches the network.
    #[must_use]
    pub fn loopback() -> Self {
        Self(config(LookupOpts::loopback()))
    }

    /// A config with every lookup on.
    #[must_use]
    pub fn public() -> Self {
        Self(config(LookupOpts::public_preset()))
    }

    /// Public preset with a 2-rung custom relay ladder, so the encode/
    /// decode path walks the variable-length relay-URL region.
    ///
    /// # Panics
    ///
    /// Panics if one of the two fixed relay URLs fails to parse.
    #[must_use]
    pub fn custom_relay() -> Self {
        let relay_a: RelayUrl = "https://a.example".parse().expect("valid relay url");
        let relay_b: RelayUrl = "https://b.example".parse().expect("valid relay url");
        let lookups = LookupOpts::new(RelayChoice::Custom(vec![relay_a, relay_b])).with_mdns(true);
        Self(config(lookups))
    }
}

fn config(lookups: LookupOpts) -> MeshConfig {
    let transport = policy::app_policy(lookups.relay_lookup != RelayChoice::Disabled);
    MeshConfig::new(lookups, None, None, transport)
}

// ── crypto / identity ───────────────────────────────────────────────

/// Derives a 32-byte secret from `seed` and `label`.
#[must_use]
pub fn derive_secret(seed: &[u8; 32], label: &[u8]) -> [u8; 32] {
    *Seed::from_bytes(*seed).derive(label).as_bytes()
}

// ── mesh id (token) round-trip — config is part of the id ──────────

/// The mesh id string for `name` under `config`.
///
/// # Panics
///
/// Panics if the engine rejects a bench config, which is always valid.
#[must_use]
pub fn mesh_token(name: &MeshName, config: &BenchConfig) -> String {
    Mesh::try_new(Seed::from_bytes([7u8; 32]), name.clone(), config.0.clone())
        .expect("a bench config is valid")
        .to_string()
}

/// Parses `token` as a locked mesh and drops the result.
pub fn mesh_decode(token: &str) {
    let _: Result<LockedMesh, _> = token.parse();
}

/// Encode then decode the config region — exercises the lookup-flags +
/// relay-URL byte codec in isolation.
pub fn config_round_trip(config: &BenchConfig) {
    let _ = MeshConfig::from_bytes(&config.0.to_bytes());
}

// ── parsing / validation ────────────────────────────────────────────
// `MeshName::new` / `Nickname::new` / `MessageBody::new` are public and
// benched directly; only `MeshId::new` is `pub(crate)`.

/// Whether `value` is a valid mesh id.
#[must_use]
pub fn mesh_id_validate(value: &str) -> bool {
    MeshId::try_new(value).is_ok()
}

// ── message serialize / deserialize ─────────────────────────────────

/// A signed message with a fixed author and mesh, ready to serialize.
#[derive(Debug)]
pub struct BenchMessage(Verified);

impl BenchMessage {
    /// A signed broadcast message that carries `body`.
    ///
    /// # Panics
    ///
    /// Panics if `body` is not a valid message body.
    #[must_use]
    pub fn sample(body: &str) -> Self {
        let name = MeshName::try_new("bench").expect("valid mesh name");
        let mesh = MeshId::try_new(&mesh_token(&name, &BenchConfig::loopback()))
            .expect("Mesh::to_string is a valid MeshId");
        let author = Nickname::try_new("bench-author").expect("valid nickname");
        let message = Message::new_app(
            &mesh,
            &author,
            habilis_network::protocol::AppFrameParams::new(
                app_tag(BROADCAST),
                None,
                None,
                MessageBody::try_new(body).expect("valid body"),
            ),
        );
        Self(message.sign(&Identity::generate()))
    }

    /// The wire bytes of this message.
    ///
    /// # Panics
    ///
    /// Panics if the message is over the size cap.
    #[must_use]
    pub fn serialize(&self) -> bytes::Bytes {
        self.0
            .serialize()
            .expect("message under size cap serializes")
            .into_bytes()
    }
}

/// Parses `bytes` as a message.
///
/// # Panics
///
/// Panics if `bytes` is not a valid message.
pub fn message_deserialize(bytes: &[u8]) {
    let _ = Message::parse(bytes).expect("round-trips its own serialize");
}
