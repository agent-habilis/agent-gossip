//! Seed-derived identities and the gossip topic.
//!
//! Concept split: this module owns the seed-derived **identity** (the
//! rendezvous keypair/ports + the gossip topic id, all computed
//! locally by every joiner). The *role* of actually binding and
//! serving the rendezvous — the **beacon** — lives in
//! [`crate::beacon`].
//!
//! The `ahs…` token carries a random 32-byte `seed` (see
//! [`crate::protocol::swarm`]). Every value the swarm needs is derived
//! from it in memory — no stored address, no file:
//!
//! - the gossip topic ([`derive_topic_id`]),
//! - a well-known **rendezvous endpoint** keypair: every joiner can
//!   compute [`rendezvous_id`] locally and bootstrap from it without
//!   ever contacting the creator,
//! - (private swarms only) a deterministic loopback port ladder, since
//!   `presets::Minimal` has no pkarr/DNS lookup to resolve
//!   `rendezvous_id` into an address.
//!
//! All derivations are domain-separated SHA-256 so the topic seed, the
//! rendezvous secret key, and the port can never collide for one
//! `seed`.

use iroh::{EndpointId, SecretKey};
use iroh_gossip::proto::TopicId;
use sha2::{Digest, Sha256};

use super::swarm::SwarmName;

/// Domain-separation prefix mixed into every seed derivation. Bumping
/// this is a wire-incompatible change (peers derive a different
/// topic/identity and never meet).
const DOMAIN: &[u8] = b"agent-habilis-swarm/v2";

/// Derive an independent 32-byte secret for `label` from the swarm
/// `seed`: `SHA256(DOMAIN ‖ [label_len] ‖ label ‖ seed)`. Each label
/// (`"rendezvous"`, `"port"`, `"topic"`, …) yields a distinct value, so
/// one seed safely feeds every derived identity.
///
/// `label` is length-prefixed so distinct labels can never produce the
/// same byte stream (e.g. `("rd","vseed")` vs `("rdv","seed")`).
#[must_use]
pub(crate) fn derive_secret(seed: &[u8; 32], label: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    hasher.update([
        u8::try_from(label.len()).expect("derive_secret labels are short ASCII constants")
    ]);
    hasher.update(label);
    hasher.update(seed);
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// The shared rendezvous endpoint secret key. Every member that
/// co-hosts the rendezvous binds an iroh endpoint to this key, so the
/// node id is stable and joiner-computable. `SecretKey::from_bytes` is
/// infallible — any 32 bytes is a valid Ed25519 secret.
#[must_use]
pub(crate) fn rendezvous_secret(seed: &[u8; 32]) -> SecretKey {
    SecretKey::from_bytes(&derive_secret(seed, b"rendezvous"))
}

/// The well-known rendezvous `EndpointId` (public key of
/// [`rendezvous_secret`]). Bootstrap target for every joiner.
#[must_use]
pub(crate) fn rendezvous_id(seed: &[u8; 32]) -> EndpointId {
    rendezvous_secret(seed).public()
}

/// Number of deterministic loopback ports derived per swarm. A single
/// 2-byte-derived port collides across independent private swarms on
/// one host often enough to matter; a ladder lets the beacon skip a
/// foreign-squatted rung instead of hanging. Claim/probe semantics:
/// see `crate::beacon` module docs. A full collision needs all
/// `RENDEZVOUS_LADDER` rungs simultaneously foreign-held — negligible.
pub(crate) const RENDEZVOUS_LADDER: usize = 8;

/// The deterministic loopback port ladder for a private swarm, in
/// preference order. Mapped into the unprivileged range
/// `1024..=65535`. Derived from one `derive_secret` digest (2 bytes per
/// rung); rungs are near-certainly distinct, and a rare in-ladder dup
/// merely wastes a rung (harmless).
#[must_use]
pub(crate) fn rendezvous_ports(seed: &[u8; 32]) -> [u16; RENDEZVOUS_LADDER] {
    const LOW: u32 = 1024;
    const SPAN: u32 = 65535 - LOW + 1; // 64512
    let digest = derive_secret(seed, b"port");
    std::array::from_fn(|index| {
        let raw = u32::from(u16::from_le_bytes([
            digest[2 * index],
            digest[2 * index + 1],
        ]));
        u16::try_from(LOW + (raw % SPAN)).expect("LOW + (_ % SPAN) <= 65535")
    })
}

/// Derive the gossip TopicId from the swarm `seed` + name + config. The
/// seed is the random 32 bytes carried in the `ahs…` token, so the topic
/// is **creator-independent**: it never depends on any node's ephemeral
/// key and survives the creator's death. The name and the canonical
/// config bytes are each length-prefixed before hashing so distinct
/// fields can never collide across the boundary.
///
/// The seed is first run through the domain-separated [`derive_secret`]
/// so a `seed` can never produce the same 32 bytes for the topic and for
/// the rendezvous key. Binding the name *and* the config means a forged
/// token (same seed, swapped name or tampered rate limit / lookups)
/// hashes to a different topic and the joiner finds no peers — so every
/// member of a swarm provably shares the same config.
pub(crate) fn derive_topic_id(seed: &[u8; 32], name: &SwarmName, config_bytes: &[u8]) -> TopicId {
    let mut hasher = Sha256::new();
    hasher.update(derive_secret(seed, b"topic"));
    hasher.update([name.len_u8()]);
    hasher.update(name.as_bytes());
    let config_len =
        u16::try_from(config_bytes.len()).expect("swarm config encodes well within a u16 length");
    hasher.update(config_len.to_le_bytes());
    hasher.update(config_bytes);
    let hash = hasher.finalize();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&hash);
    TopicId::from_bytes(bytes)
}

#[cfg(test)]
mod derive_secret_tests {
    use super::{
        RENDEZVOUS_LADDER, derive_secret, rendezvous_id, rendezvous_ports, rendezvous_secret,
    };

    const SEED_A: [u8; 32] = [7u8; 32];
    const SEED_B: [u8; 32] = [9u8; 32];

    #[test]
    fn derive_secret_is_deterministic() {
        assert_eq!(
            derive_secret(&SEED_A, b"rendezvous"),
            derive_secret(&SEED_A, b"rendezvous")
        );
    }

    #[test]
    fn derive_secret_domain_separates_by_label() {
        assert_ne!(
            derive_secret(&SEED_A, b"rendezvous"),
            derive_secret(&SEED_A, b"port")
        );
    }

    #[test]
    fn derive_secret_domain_separates_by_seed() {
        assert_ne!(
            derive_secret(&SEED_A, b"rendezvous"),
            derive_secret(&SEED_B, b"rendezvous")
        );
    }

    #[test]
    fn label_length_prefix_prevents_collision() {
        // Without the length prefix ("rd"+"vx") and ("rdv"+"x") would
        // hash the same stream. The prefix must keep them distinct.
        assert_ne!(
            derive_secret(&SEED_A, b"rdvx"),
            derive_secret(&SEED_A, b"rdv")
        );
    }

    #[test]
    fn rendezvous_id_is_stable_for_a_seed() {
        assert_eq!(rendezvous_id(&SEED_A), rendezvous_id(&SEED_A));
        assert_ne!(rendezvous_id(&SEED_A), rendezvous_id(&SEED_B));
    }

    #[test]
    fn rendezvous_id_matches_secret_public() {
        assert_eq!(rendezvous_id(&SEED_A), rendezvous_secret(&SEED_A).public());
    }

    #[test]
    fn rendezvous_ports_are_in_range_stable_and_seed_specific() {
        let ladder = rendezvous_ports(&SEED_A);
        assert_eq!(ladder.len(), RENDEZVOUS_LADDER);
        assert!(ladder.iter().all(|&port| port >= 1024));
        assert_eq!(ladder, rendezvous_ports(&SEED_A), "stable for a seed");
        assert_ne!(
            rendezvous_ports(&SEED_A),
            rendezvous_ports(&SEED_B),
            "different seeds get different ladders"
        );
    }

    #[test]
    fn rendezvous_ports_rungs_are_near_certainly_distinct() {
        let ladder = rendezvous_ports(&SEED_A);
        let unique: std::collections::HashSet<_> = ladder.iter().collect();
        // A rare in-ladder dup is harmless, but the fixed test seed
        // must not regress into a degenerate all-same ladder.
        assert!(unique.len() >= RENDEZVOUS_LADDER - 1);
    }
}

#[cfg(test)]
mod topic_tests {
    use super::{SwarmName, derive_topic_id};

    fn name(text: &str) -> SwarmName {
        SwarmName::new(text).unwrap()
    }

    const CFG_A: &[u8] = &[60, 0, 0]; // rate 60, lookups loopback
    const CFG_B: &[u8] = &[0, 0, 0b0111]; // rate 0, mdns+dht+pinned relay

    #[test]
    fn deterministic_for_same_input() {
        let seed = [42u8; 32];
        let first = derive_topic_id(&seed, &name("team"), CFG_A);
        let second = derive_topic_id(&seed, &name("team"), CFG_A);
        assert_eq!(first, second);
    }

    #[test]
    fn different_seeds_produce_different_topics() {
        let seed_a = [1u8; 32];
        let seed_b = [42u8; 32];
        assert_ne!(
            derive_topic_id(&seed_a, &name("team"), CFG_A),
            derive_topic_id(&seed_b, &name("team"), CFG_A)
        );
    }

    #[test]
    fn different_names_produce_different_topics() {
        let seed = [1u8; 32];
        assert_ne!(
            derive_topic_id(&seed, &name("alpha"), CFG_A),
            derive_topic_id(&seed, &name("beta"), CFG_A)
        );
    }

    #[test]
    fn different_config_produces_different_topics() {
        let seed = [1u8; 32];
        assert_ne!(
            derive_topic_id(&seed, &name("team"), CFG_A),
            derive_topic_id(&seed, &name("team"), CFG_B),
            "config is mixed into the topic, so it cannot diverge across members"
        );
    }
}
