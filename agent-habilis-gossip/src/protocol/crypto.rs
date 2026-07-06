//! Seed-derived identities and the gossip topic.
//!
//! Concept split: this module owns the seed-derived **identity** (the
//! rendezvous keypair/ports + the gossip topic id, all computed
//! locally by every joiner). The *role* of actually binding and
//! serving the rendezvous — the **beacon** — lives in
//! [`crate::beacon`].
//!
//! The `💬…` token carries a random 32-byte `seed` (see
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
use crate::util::consts::{PASSWORD_KDF_M_COST_KIB, PASSWORD_KDF_P_COST, PASSWORD_KDF_T_COST};

/// Domain-separation prefix mixed into every seed derivation. Bumping
/// this is a wire-incompatible change (peers derive a different
/// topic/identity and never meet).
const DOMAIN: &[u8] = b"agent-gossip/v2";

/// Domain-separation prefix for the topic-string → seed derivation
/// ([`topic_seed`]). Distinct from [`DOMAIN`] so an arbitrary user string can
/// never reproduce a [`derive_secret`] output. Versioned independently:
/// bumping it makes every string derive a fresh swarm.
const TOPIC_DOMAIN: &[u8] = b"agent-gossip/topic-seed/v1";

/// Deterministically derive a swarm `seed` from an arbitrary string:
/// `SHA256(TOPIC_DOMAIN ‖ topic.trim())`. Two callers passing the same string
/// (byte-for-byte after trimming surrounding whitespace) land on the same seed
/// — and thus, with the same name + config, the same swarm.
///
/// Trim-only by design: the string is *not* lowercased or URL-normalized. URL
/// paths are case-sensitive and Unicode case-folding is platform-dependent, so
/// either would silently break convergence across machines.
#[must_use]
pub(crate) fn topic_seed(topic: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(TOPIC_DOMAIN);
    hasher.update(topic.trim().as_bytes());
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

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

/// An operator-supplied password. A newtype so a stray `{:?}` in a
/// `tracing` call can never leak the value into a log file that is
/// otherwise safe to share. Its bytes are wiped on drop (best-effort: any
/// `String` reallocation or the CLI prompt buffer it was built from is not
/// tracked), so a retained password does not linger in freed memory.
#[derive(Clone, PartialEq, Eq)]
pub struct Password(String);

impl Password {
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Drop for Password {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.0);
    }
}

impl std::fmt::Debug for Password {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("***")
    }
}

impl std::fmt::Display for Password {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("***")
    }
}

/// Argon2id-stretch a password into 32 bytes of keying material. The salt is
/// `derive_secret(salt_seed, label)` — keyed by the swarm's random seed and
/// domain-separated by `label`, so the stretch shares no salt with any other
/// `derive_secret` use and no rainbow table carries over. Cost params are a
/// wire contract (`util::consts`); ~50-100ms by design, paid once at
/// create/join.
fn stretch_password(password: &Password, salt_seed: &[u8; 32], label: &[u8]) -> [u8; 32] {
    let params = argon2::Params::new(
        PASSWORD_KDF_M_COST_KIB,
        PASSWORD_KDF_T_COST,
        PASSWORD_KDF_P_COST,
        Some(32),
    )
    .expect("compile-time KDF params are valid");
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let salt = derive_secret(salt_seed, label);
    let mut out = [0u8; 32];
    argon
        .hash_password_into(password.as_str().as_bytes(), &salt, &mut out)
        .expect("32-byte salt and output are valid argon2 lengths");
    out
}

/// The stretched key for a passworded swarm: every derivation (topic,
/// rendezvous, port ladder) switches from the wire seed onto this value, so
/// holding the `💬…` id without the password computes nothing reachable.
#[must_use]
pub(crate) fn stretch_swarm_password(password: &Password, seed: &[u8; 32]) -> [u8; 32] {
    stretch_password(password, seed, b"password")
}

/// The 32-byte key that wraps an invite ticket's root secret when the swarm's
/// creator minted it with a password. Argon2id-stretched from the password,
/// salted by the swarm's public identity salt (the wire seed of an invite-only
/// swarm — public, in the hash, so both minter and redeemer derive the same key
/// without already holding the root). Domain-separated from the swarm-password
/// and ticket labels so no stretch is shared. The wrapped root is
/// `seal_symmetric`ed under this key, so a redeemer without the password can't
/// recover it — bullet: a leaked invite still needs the password.
#[must_use]
pub(crate) fn invite_wrap_key(password: &Password, salt: &[u8; 32]) -> [u8; 32] {
    stretch_password(password, salt, b"invite-ticket")
}

/// Length of the password verifier carried in a passworded swarm hash.
/// 16 bytes: only guess-checking matters (collisions are irrelevant), and
/// every extra byte grows the shared `💬…` string.
pub(crate) const PASSWORD_VERIFIER_LEN: usize = 16;

/// The one-way check value a passworded swarm hash carries so `join` can
/// verify a candidate password locally. Derived from the *stretched* key
/// through a second one-way step, so the verifier reveals neither the
/// password nor the effective seed.
#[must_use]
pub(crate) fn password_verifier(key: &[u8; 32]) -> [u8; PASSWORD_VERIFIER_LEN] {
    let digest = derive_secret(key, b"password-verify");
    let mut out = [0u8; PASSWORD_VERIFIER_LEN];
    out.copy_from_slice(&digest[..PASSWORD_VERIFIER_LEN]);
    out
}

/// The 32 bytes a bridge consumer presents on connect and a producer
/// expects: the raw ticket secret (no password), or its Argon2id stretch
/// salted by the secret (passworded) — same wire size either way, so a
/// directory ad (which carries the secret) is not enough to redeem a
/// passworded ticket.
#[derive(Clone)]
pub struct TicketAuth {
    pub token: [u8; 32],
    pub password_protected: bool,
}

impl TicketAuth {
    /// The token for an **a2a bridge** ticket (pipe/port).
    #[must_use]
    pub fn a2a(secret: &[u8; 32], password: Option<&Password>) -> Self {
        Self::derive(secret, password, b"a2a-ticket")
    }

    /// The token for a **blob** ticket. Domain-separated from the a2a bridge so
    /// the two ticket kinds never derive the same passworded token even from a
    /// shared secret.
    pub(crate) fn blob(secret: &[u8; 32], password: Option<&Password>) -> Self {
        Self::derive(secret, password, b"blob-ticket")
    }

    fn derive(secret: &[u8; 32], password: Option<&Password>, label: &[u8]) -> Self {
        match password {
            Some(password) => Self {
                token: stretch_password(password, secret, label),
                password_protected: true,
            },
            None => Self {
                token: *secret,
                password_protected: false,
            },
        }
    }
}

impl std::fmt::Debug for TicketAuth {
    // The token is the live credential — redact it, keep the flag.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TicketAuth")
            .field("token", &"***")
            .field("password_protected", &self.password_protected)
            .finish()
    }
}

/// Constant-time equality for 32-byte auth tokens: XOR-fold the full pair so
/// the cost never depends on where the first mismatch lies (a short-circuit
/// `==` is a timing oracle on the secret).
#[must_use]
pub fn ct_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0u8, |acc, (lhs, rhs)| acc | (lhs ^ rhs))
        == 0
}

/// Derive the gossip TopicId from the swarm `seed` + name + config. The
/// seed is the random 32 bytes carried in the `💬…` token, so the topic
/// is **creator-independent**: it never depends on any node's ephemeral
/// key and survives the creator's death. The name and the canonical
/// config bytes are each length-prefixed before hashing so distinct
/// fields can never collide across the boundary.
///
/// The seed is first run through the domain-separated [`derive_secret`]
/// so a `seed` can never produce the same 32 bytes for the topic and for
/// the rendezvous key. Binding the name *and* the config means a forged
/// token (same seed, swapped name or tampered lookups)
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
        topic_seed,
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
    fn topic_seed_is_deterministic() {
        assert_eq!(topic_seed("agent-habilis"), topic_seed("agent-habilis"));
    }

    #[test]
    fn topic_seed_differs_by_string() {
        assert_ne!(topic_seed("alpha"), topic_seed("beta"));
    }

    #[test]
    fn topic_seed_trims_surrounding_whitespace() {
        assert_eq!(topic_seed("  x\n"), topic_seed("x"));
    }

    #[test]
    fn topic_seed_is_case_sensitive() {
        assert_ne!(topic_seed("Repo"), topic_seed("repo"));
    }

    #[test]
    fn topic_seed_domain_separated_from_derive_secret() {
        // A topic seed lives under TOPIC_DOMAIN; it must not coincide with a
        // labelled derivation off some other seed for any plausible collision.
        assert_ne!(
            topic_seed("rendezvous"),
            derive_secret(&SEED_A, b"rendezvous")
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
mod password_tests {
    use super::{Password, derive_secret, password_verifier, stretch_swarm_password};

    const SEED_A: [u8; 32] = [7u8; 32];
    const SEED_B: [u8; 32] = [9u8; 32];

    fn pw(text: &str) -> Password {
        Password::new(text.to_owned())
    }

    #[test]
    fn stretch_is_deterministic_and_separates_by_password_and_seed() {
        let key = stretch_swarm_password(&pw("hunter2"), &SEED_A);
        assert_eq!(key, stretch_swarm_password(&pw("hunter2"), &SEED_A));
        assert_ne!(key, stretch_swarm_password(&pw("hunter3"), &SEED_A));
        assert_ne!(key, stretch_swarm_password(&pw("hunter2"), &SEED_B));
    }

    #[test]
    fn verifier_is_not_the_key_and_not_a_topic_or_rendezvous_input() {
        let key = stretch_swarm_password(&pw("hunter2"), &SEED_A);
        let verifier = password_verifier(&key);
        assert_ne!(&key[..16], &verifier[..], "verifier must be one-way");
        assert_ne!(
            &derive_secret(&key, b"topic")[..16],
            &verifier[..],
            "verifier must not collide with a derivation label"
        );
    }

    #[test]
    fn password_never_leaks_through_debug_or_display() {
        let secret = pw("hunter2");
        assert_eq!(format!("{secret:?}"), "***");
        assert_eq!(format!("{secret}"), "***");
    }

    #[test]
    fn channel_keys_are_domain_separated() {
        // The state/meta/broadcast content keys are all derived from the same
        // stretched swarm key but must never collide.
        let swarm_key = stretch_swarm_password(&pw("hunter2"), &SEED_A);
        let state = derive_secret(&swarm_key, b"state-doc");
        let meta = derive_secret(&swarm_key, b"meta-doc");
        let broadcast = derive_secret(&swarm_key, b"broadcast");
        assert_ne!(state, meta);
        assert_ne!(state, broadcast);
        assert_ne!(meta, broadcast);
    }

    #[test]
    fn blob_and_a2a_ticket_tokens_are_domain_separated() {
        use super::TicketAuth;
        let secret = [3u8; 32];
        let password = pw("hunter2");
        let blob = TicketAuth::blob(&secret, Some(&password)).token;
        let a2a = TicketAuth::a2a(&secret, Some(&password)).token;
        assert_ne!(
            blob, a2a,
            "the same password+secret must yield different tokens per ticket kind"
        );
        // Without a password both are the raw secret (no stretch, no label).
        assert_eq!(TicketAuth::blob(&secret, None).token, secret);
        assert_eq!(TicketAuth::a2a(&secret, None).token, secret);
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
