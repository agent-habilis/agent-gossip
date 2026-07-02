//! The swarm identifier, at two levels:
//!
//! - [`SwarmId`] — the validated `🐝…` *string* (shallow: prefix +
//!   length + Base58 charset). Cheap boundary check at the CLI / IPC
//!   edge. Code: [`id`].
//! - [`Swarm`] — the *decoded* structure (32-byte seed + name +
//!   [`SwarmConfig`]), with the Base58Check codec (this file). `SwarmId`
//!   is what flows through the wire/CLI; `Swarm` is what `setup_swarm`
//!   derives identity and behavior from.
//!
//! See `docs/swarm-hash.md` for the full byte layout and rationale.
//!
//! Also home to [`SwarmName`] ([`name`]), the lookup allowlist ([`lookup`])
//! carried in the id, and the relay-ladder parsing.

use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use iroh::EndpointId;
use sha2::{Digest, Sha256};

use super::crypto;

mod id;
mod lookup;
mod name;

pub use id::{SwarmId, SwarmIdError};
pub(crate) use lookup::{
    AdvertiseRequiresReachable, DEFAULT_DIRECTORY, DirectorySelection, LookupOpts, RelayChoice,
    SwarmConfig, resolve_lookups, resolve_transfer_lookups, validate_advertise,
};
pub use lookup::{LookupSet, RelayLadder, RelayLadderError, RelaySelection};
pub use name::{NameError, SwarmName};

const PREFIX: &str = "🐝";

/// Id format version. A single byte reserved so the encoding can evolve;
/// an unknown version is rejected.
const VERSION: u8 = 1;
const SEED_LEN: usize = 32;
/// Wire bound for the encoded name in bytes. `SwarmName::new` caps the
/// name at `ident::MAX_CHARS` scalar values; each is at most 4 UTF-8
/// bytes, so the encoded form fits this many bytes (and inside the
/// 1-byte length field).
const NAME_MAX_BYTES: usize = super::ident::MAX_CHARS * 4;

/// A swarm identifier — Base58Check payload with a `🐝` prefix.
///
/// The token carries the random `seed` plus the swarm's [`SwarmConfig`]
/// (lookups); **no peer address is ever stored**. The
/// gossip topic, the well-known rendezvous identity (every joiner's
/// bootstrap target), and the loopback port ladder are all derived from
/// `seed` in memory, so the swarm is creator-independent and survives
/// the creator's death. The config is mixed into the topic derivation,
/// so every member of a swarm shares the same config.
///
/// Wire format (little-endian):
///
/// ```text
/// [1 byte version]
/// [32 bytes seed]
/// [1 byte name length in bytes, 1..=128]
/// [N bytes name (UTF-8, <=32 scalars, charset enforced by SwarmName)]
/// [2 bytes config length] [config bytes (see SwarmConfig::to_bytes)]
/// ```
#[derive(Debug, Clone)]
pub(crate) struct Swarm {
    pub name: SwarmName,
    seed: [u8; SEED_LEN],
    /// The Argon2id-stretched password key, once a password is applied.
    /// Every derivation (topic, rendezvous, port ladder) switches onto it,
    /// so holding the id without the password computes nothing reachable.
    /// Never serialized — `encode_bytes` writes `seed`.
    stretched_key: Option<[u8; SEED_LEN]>,
    pub config: SwarmConfig,
}

impl Swarm {
    pub(crate) fn new(seed: [u8; SEED_LEN], name: SwarmName, config: SwarmConfig) -> Self {
        Swarm {
            name,
            seed,
            stretched_key: None,
            config,
        }
    }

    /// Build a swarm deterministically from an arbitrary string (the `forum`
    /// command). The seed is [`crypto::topic_seed`] of the string; the name is
    /// the string itself sanitized into a [`SwarmName`]
    /// ([`SwarmName::from_topic_string`]). Both are deterministic functions of
    /// the string, so callers passing the same string + config converge on the
    /// same id with zero coordination.
    pub(crate) fn from_topic(topic: &str, config: SwarmConfig) -> Self {
        let seed = crypto::topic_seed(topic);
        let name = SwarmName::from_topic_string(topic);
        Swarm {
            name,
            seed,
            stretched_key: None,
            config,
        }
    }

    /// The wire seed (what the encoded id carries). Test-only: production
    /// derivations go through [`Swarm::effective_seed`] so the password mix
    /// can never be bypassed.
    #[cfg(test)]
    pub(crate) fn seed(&self) -> &[u8; SEED_LEN] {
        &self.seed
    }

    /// The seed every derivation uses: the stretched password key when one
    /// is applied, else the wire seed. Asserts a passworded swarm never
    /// derives before `apply_password`/`set_password` — deriving from the
    /// raw seed would silently land in an unreachable topic, which is
    /// strictly worse than failing loudly.
    fn effective_seed(&self) -> &[u8; SEED_LEN] {
        if let Some(key) = &self.stretched_key {
            key
        } else {
            assert!(
                self.config.password.is_none(),
                "passworded swarm derived before the password was applied"
            );
            &self.seed
        }
    }

    /// Whether the id carries a password verifier (joiners must present
    /// the password).
    pub(crate) fn requires_password(&self) -> bool {
        self.config.password.is_some()
    }

    /// Join-side: stretch `password` (salt = the wire seed), check it
    /// against the verifier the id carries, and switch every derivation
    /// onto the stretched key. ~100ms of Argon2id by design.
    ///
    /// # Errors
    /// The id carries no verifier (passwordless swarm), or the password is
    /// wrong.
    pub(crate) fn apply_password(&mut self, password: &crypto::Password) -> Result<()> {
        let Some(expected) = &self.config.password else {
            bail!("this swarm has no password — drop --password");
        };
        let key = crypto::stretch_swarm_password(password, &self.seed);
        if crypto::password_verifier(&key) != *expected {
            bail!("wrong password");
        }
        self.stretched_key = Some(key);
        Ok(())
    }

    /// Create-side: stretch `password`, bake its verifier into the config
    /// (the encoded id must carry it), and switch every derivation onto
    /// the stretched key. ~100ms of Argon2id by design.
    pub(crate) fn set_password(&mut self, password: &crypto::Password) {
        let key = crypto::stretch_swarm_password(password, &self.seed);
        self.config.password = Some(crypto::password_verifier(&key));
        self.stretched_key = Some(key);
    }

    /// The gossip `TopicId` — the one derivation entry point, so no caller
    /// can forget the password mix.
    pub(crate) fn topic_id(&self) -> iroh_gossip::proto::TopicId {
        crypto::derive_topic_id(self.effective_seed(), &self.name, &self.config_bytes())
    }

    /// The shared rendezvous endpoint secret every co-host binds.
    pub(crate) fn rendezvous_secret(&self) -> iroh::SecretKey {
        crypto::rendezvous_secret(self.effective_seed())
    }

    pub(crate) fn lookups(&self) -> &LookupOpts {
        &self.config.lookups
    }

    /// True when the swarm is loopback-only (no off-machine lookups).
    pub(crate) fn is_loopback(&self) -> bool {
        self.config.lookups.is_loopback()
    }

    /// `"private"`/`"public"` label for output, derived from the lookups.
    pub(crate) fn network_label(&self) -> &'static str {
        self.config.lookups.network_label()
    }

    /// The canonical config bytes mixed into the topic derivation (so a
    /// swarm's config is part of its identity).
    pub(crate) fn config_bytes(&self) -> Vec<u8> {
        self.config.to_bytes()
    }

    /// Well-known rendezvous `EndpointId`, derived from the effective seed.
    /// Every joiner computes this locally and bootstraps gossip from it; it
    /// is co-hosted by members rather than pinned to the creator.
    pub(crate) fn rendezvous_id(&self) -> EndpointId {
        crypto::rendezvous_id(self.effective_seed())
    }

    /// Deterministic loopback port *ladder* for loopback-only swarms (no
    /// pkarr/DNS to resolve `rendezvous_id`). Preference order; a beacon
    /// binds the first free rung, joiners try all rungs.
    pub(crate) fn rendezvous_ports(&self) -> [u16; crypto::RENDEZVOUS_LADDER] {
        crypto::rendezvous_ports(self.effective_seed())
    }

    fn encode_bytes(&self) -> Vec<u8> {
        let config = self.config.to_bytes();
        let mut buf =
            Vec::with_capacity(1 + SEED_LEN + 1 + self.name.as_bytes().len() + 2 + config.len());
        buf.push(VERSION);
        buf.extend_from_slice(&self.seed);
        // SwarmName guarantees 1..=128 UTF-8 bytes, so a 1-byte length is safe.
        buf.push(self.name.len_u8());
        buf.extend_from_slice(self.name.as_bytes());
        let config_len =
            u16::try_from(config.len()).expect("SwarmConfig encodes well within a u16 length");
        buf.extend_from_slice(&config_len.to_le_bytes());
        buf.extend_from_slice(&config);
        buf
    }

    fn decode_bytes(bytes: &[u8]) -> Result<Self> {
        let mut pos = 0usize;
        let version = *bytes.get(pos).context("Swarm identifier too short")?;
        pos += 1;
        if version != VERSION {
            bail!("Unsupported swarm id version: {version}");
        }

        let seed_slice = bytes
            .get(pos..pos + SEED_LEN)
            .context("Swarm identifier too short")?;
        let mut seed = [0u8; SEED_LEN];
        seed.copy_from_slice(seed_slice);
        pos += SEED_LEN;

        let name_len = *bytes.get(pos).context("Truncated swarm name length")? as usize;
        pos += 1;
        if name_len == 0 || name_len > NAME_MAX_BYTES {
            bail!("Invalid swarm name length: {name_len}");
        }
        let name_raw = bytes
            .get(pos..pos + name_len)
            .context("Truncated swarm name")?;
        pos += name_len;
        let name_str = std::str::from_utf8(name_raw).context("Invalid swarm name UTF-8")?;
        let name = SwarmName::new(name_str).context("Invalid swarm name")?;

        let config_len =
            lookup::read_u16(bytes, &mut pos).context("Truncated config length")? as usize;
        let config_raw = bytes
            .get(pos..pos + config_len)
            .context("Truncated swarm config")?;
        pos += config_len;
        if pos != bytes.len() {
            bail!("Trailing bytes in swarm identifier");
        }
        let config = SwarmConfig::from_bytes(config_raw)?;

        Ok(Swarm {
            name,
            seed,
            stretched_key: None,
            config,
        })
    }
}

impl fmt::Display for Swarm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = self.encode_bytes();
        let encoded = base58check_encode(&bytes);
        write!(f, "{PREFIX}{encoded}")
    }
}

fn checksum(bytes: &[u8]) -> [u8; 4] {
    let first = Sha256::digest(bytes);
    let second = Sha256::digest(first);
    let mut out = [0u8; 4];
    out.copy_from_slice(&second[..4]);
    out
}

fn base58check_encode(payload: &[u8]) -> String {
    let mut with_checksum = payload.to_vec();
    with_checksum.extend_from_slice(&checksum(payload));
    bs58::encode(with_checksum).into_string()
}

fn base58check_decode(encoded: &str) -> Result<Vec<u8>> {
    let decoded = bs58::decode(encoded)
        .into_vec()
        .context("Invalid Base58 swarm encoding")?;
    if decoded.len() < 4 {
        bail!("Swarm identifier too short");
    }
    let (payload, received_checksum) = decoded.split_at(decoded.len() - 4);
    let expected_checksum = checksum(payload);
    if received_checksum != expected_checksum {
        bail!("Invalid swarm checksum");
    }
    Ok(payload.to_vec())
}

impl FromStr for Swarm {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let payload = s
            .strip_prefix(PREFIX)
            .context("Invalid swarm prefix: expected '🐝'")?;
        let bytes = base58check_decode(payload)?;
        Self::decode_bytes(&bytes)
    }
}

#[cfg(test)]
mod swarm_tests {
    use super::{LookupOpts, RelayChoice, SEED_LEN, Swarm, SwarmConfig, SwarmName};

    fn dummy_seed() -> [u8; SEED_LEN] {
        [7u8; SEED_LEN]
    }

    fn dummy_name() -> SwarmName {
        SwarmName::new("test").unwrap()
    }

    fn custom_config() -> SwarmConfig {
        SwarmConfig {
            lookups: LookupOpts {
                mdns: true,
                dht: false,
                relay: RelayChoice::Custom(vec![
                    "https://a.example".parse().unwrap(),
                    "https://b.example".parse().unwrap(),
                ]),
            },
            password: None,
        }
    }

    #[test]
    fn round_trip_loopback() {
        let swarm = Swarm::new(dummy_seed(), dummy_name(), SwarmConfig::loopback());
        let encoded = swarm.to_string();
        assert!(encoded.starts_with("🐝"));
        let decoded: Swarm = encoded.parse().unwrap();
        assert_eq!(decoded.seed(), swarm.seed());
        assert_eq!(decoded.name, swarm.name);
        assert_eq!(decoded.config, swarm.config);
        assert!(decoded.is_loopback());
    }

    #[test]
    fn round_trip_public_preset() {
        let swarm = Swarm::new(
            [0xABu8; SEED_LEN],
            dummy_name(),
            SwarmConfig::public_preset(),
        );
        let decoded: Swarm = swarm.to_string().parse().unwrap();
        assert_eq!(decoded.seed(), &[0xABu8; SEED_LEN]);
        assert_eq!(decoded.config, swarm.config);
        assert!(!decoded.is_loopback());
        assert_eq!(decoded.network_label(), "public");
    }

    #[test]
    fn round_trip_custom_relay_ladder() {
        let swarm = Swarm::new(dummy_seed(), dummy_name(), custom_config());
        let decoded: Swarm = swarm.to_string().parse().unwrap();
        assert_eq!(decoded.config, custom_config());
    }

    #[test]
    fn seed_drives_rendezvous_identity() {
        let swarm = Swarm::new(dummy_seed(), dummy_name(), SwarmConfig::public_preset());
        let decoded: Swarm = swarm.to_string().parse().unwrap();
        assert_eq!(decoded.rendezvous_id(), swarm.rendezvous_id());
        assert_eq!(decoded.rendezvous_ports(), swarm.rendezvous_ports());
    }

    #[test]
    fn different_seeds_yield_different_ids() {
        let one = Swarm::new([1u8; SEED_LEN], dummy_name(), SwarmConfig::loopback()).to_string();
        let two = Swarm::new([2u8; SEED_LEN], dummy_name(), SwarmConfig::loopback()).to_string();
        assert_ne!(one, two);
    }

    #[test]
    fn different_config_yields_different_id() {
        let one = Swarm::new(dummy_seed(), dummy_name(), SwarmConfig::loopback()).to_string();
        let two = Swarm::new(dummy_seed(), dummy_name(), SwarmConfig::public_preset()).to_string();
        assert_ne!(one, two, "config is part of the id");
    }

    #[test]
    fn password_verifier_yields_different_id_and_round_trips() {
        let passworded = SwarmConfig {
            lookups: LookupOpts::public_preset(),
            password: Some([0x5Au8; 16]),
        };
        let one = Swarm::new(dummy_seed(), dummy_name(), SwarmConfig::public_preset());
        let two = Swarm::new(dummy_seed(), dummy_name(), passworded.clone());
        assert_ne!(one.to_string(), two.to_string());
        let decoded: Swarm = two.to_string().parse().unwrap();
        assert_eq!(decoded.config, passworded);
    }

    #[test]
    fn set_password_bakes_verifier_and_apply_verifies_it() {
        use crate::protocol::crypto::Password;
        let password = Password::new("hunter2".to_owned());

        let mut creator = Swarm::new(dummy_seed(), dummy_name(), SwarmConfig::public_preset());
        let passwordless_topic = creator.topic_id();
        let passwordless_rendezvous = creator.rendezvous_id();
        creator.set_password(&password);
        assert!(creator.requires_password());

        // Every derivation switched onto the stretched key.
        assert_ne!(creator.topic_id(), passwordless_topic);
        assert_ne!(creator.rendezvous_id(), passwordless_rendezvous);

        // A joiner decoding the id verifies and lands on identical derivations.
        let mut joiner: Swarm = creator.to_string().parse().unwrap();
        assert!(joiner.requires_password());
        joiner.apply_password(&password).unwrap();
        assert_eq!(joiner.topic_id(), creator.topic_id());
        assert_eq!(joiner.rendezvous_id(), creator.rendezvous_id());
        assert_eq!(joiner.rendezvous_ports(), creator.rendezvous_ports());

        // Wrong password fails locally, against the verifier.
        let mut wrong: Swarm = creator.to_string().parse().unwrap();
        let error = wrong
            .apply_password(&Password::new("hunter3".to_owned()))
            .unwrap_err();
        assert!(error.to_string().contains("wrong password"), "got: {error}");
    }

    #[test]
    fn apply_password_on_passwordless_id_is_rejected() {
        use crate::protocol::crypto::Password;
        let mut swarm = Swarm::new(dummy_seed(), dummy_name(), SwarmConfig::public_preset());
        let error = swarm
            .apply_password(&Password::new("hunter2".to_owned()))
            .unwrap_err();
        assert!(error.to_string().contains("no password"), "got: {error}");
    }

    #[test]
    #[should_panic(expected = "passworded swarm derived before the password was applied")]
    fn passworded_swarm_refuses_to_derive_without_the_password() {
        let mut swarm = Swarm::new(dummy_seed(), dummy_name(), SwarmConfig::public_preset());
        swarm.config.password = Some([1u8; 16]);
        let _ = swarm.topic_id();
    }

    #[test]
    fn golden_passwordless_id_and_topic_are_pinned() {
        // Byte-for-byte back-compat pin: a passwordless id and its topic
        // must never change encoding (live swarms depend on it). If this
        // fails, the wire format regressed — do not update the constants
        // without a deliberate, versioned format change.
        let swarm = Swarm::new(dummy_seed(), dummy_name(), SwarmConfig::public_preset());
        assert_eq!(
            swarm.to_string(),
            "🐝2UXAThUkdBAbiJNXvCt4YeMGQ9myFg7gJJZSr3pG3MAGzUwWmmV7D2NgrWBn1"
        );
        let topic =
            super::crypto::derive_topic_id(swarm.seed(), &swarm.name, &swarm.config_bytes());
        assert_eq!(
            format!("{topic:?}"),
            "TopicId(3d20258943e8604421caaabcc59fca2a0d86ef87f5c6976cf33a3a32005a14b3)"
        );
    }

    #[test]
    fn from_topic_is_deterministic() {
        let one = Swarm::from_topic("agent-habilis", SwarmConfig::public_preset());
        let two = Swarm::from_topic("agent-habilis", SwarmConfig::public_preset());
        assert_eq!(one.to_string(), two.to_string());
    }

    #[test]
    fn from_topic_name_is_the_sanitized_string() {
        let swarm = Swarm::from_topic(
            "https://github.com/agent-habilis/swarm",
            SwarmConfig::public_preset(),
        );
        // Scheme stripped, `/`s kept, 30 chars ≤ 32 so no truncation.
        assert_eq!(swarm.name.as_str(), "github.com/agent-habilis/swarm");
    }

    #[test]
    fn from_topic_differs_by_string() {
        let one = Swarm::from_topic("alpha", SwarmConfig::public_preset());
        let two = Swarm::from_topic("beta", SwarmConfig::public_preset());
        assert_ne!(one.to_string(), two.to_string());
    }

    #[test]
    fn from_topic_round_trips_through_from_str() {
        let swarm = Swarm::from_topic("agent-habilis", SwarmConfig::public_preset());
        let decoded: Swarm = swarm.to_string().parse().expect("decode failed");
        assert_eq!(decoded.seed(), swarm.seed());
        assert_eq!(decoded.name, swarm.name);
        assert_eq!(decoded.config, swarm.config);
    }

    #[test]
    fn invalid_prefix_rejected() {
        let encoded = Swarm::new(dummy_seed(), dummy_name(), SwarmConfig::loopback()).to_string();
        let bad = format!("xxx{}", &encoded[super::PREFIX.len()..]);
        assert!(bad.parse::<Swarm>().is_err());
    }

    #[test]
    fn non_bee_prefix_rejected() {
        let encoded = Swarm::new(dummy_seed(), dummy_name(), SwarmConfig::loopback()).to_string();
        for bad_prefix in ["sw1", "xyz", "ahs"] {
            let bad = format!("{}{}", bad_prefix, &encoded[super::PREFIX.len()..]);
            assert!(
                bad.parse::<Swarm>().is_err(),
                "expected reject for prefix {bad_prefix}",
            );
        }
    }

    #[test]
    fn invalid_checksum_rejected() {
        let encoded = Swarm::new(dummy_seed(), dummy_name(), SwarmConfig::loopback()).to_string();
        let mut bad = encoded.clone();
        let last_index = bad.len() - 1;
        let replacement = if bad.ends_with('1') { "2" } else { "1" };
        bad.replace_range(last_index.., replacement);
        assert!(bad.parse::<Swarm>().is_err());
    }

    #[test]
    fn truncated_bytes_rejected() {
        assert!(Swarm::decode_bytes(&[0u8; 10]).is_err());
    }

    #[test]
    fn unknown_version_rejected() {
        let swarm = Swarm::new(dummy_seed(), dummy_name(), SwarmConfig::loopback());
        let mut bytes = swarm.encode_bytes();
        bytes[0] = 2; // an unknown version byte
        assert!(Swarm::decode_bytes(&bytes).is_err());
    }

    #[test]
    fn swarm_id_round_trips_unicode_name() {
        let name = SwarmName::new("café-日本-🐝").unwrap();
        let swarm = Swarm::new(dummy_seed(), name.clone(), SwarmConfig::loopback());
        let decoded: Swarm = swarm.to_string().parse().expect("decode failed");
        assert_eq!(decoded.name, name);
    }

    #[test]
    fn swarm_id_round_trips_max_byte_name() {
        // 32 four-byte scalars = 128 bytes = the most the 1-byte name
        // length field can carry; exercises the encode/decode upper edge.
        let name = SwarmName::new("🐝".repeat(32)).unwrap();
        assert_eq!(name.as_bytes().len(), 128);
        let swarm = Swarm::new(dummy_seed(), name.clone(), SwarmConfig::public_preset());
        let decoded: Swarm = swarm.to_string().parse().expect("decode failed");
        assert_eq!(decoded.name, name);
    }

    mod prop {
        use proptest::{
            array::{uniform16, uniform32},
            prelude::any,
            prop_assert, prop_assert_eq, prop_assert_ne, prop_assume, proptest,
            strategy::Strategy,
        };

        use super::{SEED_LEN, Swarm, SwarmConfig, SwarmName};

        fn arb_seed() -> impl Strategy<Value = [u8; SEED_LEN]> {
            uniform32(0u8..)
        }

        fn arb_name() -> impl Strategy<Value = SwarmName> {
            "[a-z][a-z0-9_-]{0,31}".prop_map(|raw| SwarmName::new(raw).unwrap())
        }

        fn arb_config() -> impl Strategy<Value = SwarmConfig> {
            (any::<bool>(), proptest::option::of(uniform16(0u8..))).prop_map(
                |(public, verifier)| {
                    let mut config = if public {
                        SwarmConfig::public_preset()
                    } else {
                        SwarmConfig::loopback()
                    };
                    config.password = verifier;
                    config
                },
            )
        }

        proptest! {
            #![proptest_config(crate::proptest_support::config())]
            #[test]
            fn prop_round_trip(
                seed in arb_seed(),
                name in arb_name(),
                config in arb_config(),
            ) {
                let swarm = Swarm::new(seed, name.clone(), config.clone());
                let decoded: Swarm = swarm.to_string().parse().expect("decode failed");
                prop_assert_eq!(decoded.seed(), swarm.seed());
                prop_assert_eq!(decoded.name, swarm.name);
                prop_assert_eq!(decoded.config, config);
            }

            #[test]
            fn prop_prefix(seed in arb_seed(), name in arb_name()) {
                let swarm = Swarm::new(seed, name, SwarmConfig::loopback());
                prop_assert!(swarm.to_string().starts_with("🐝"));
            }

            #[test]
            fn prop_deterministic(
                seed in arb_seed(),
                name in arb_name(),
                config in arb_config(),
            ) {
                let swarm = Swarm::new(seed, name, config);
                prop_assert_eq!(swarm.to_string(), swarm.to_string());
            }

            #[test]
            fn prop_distinct_seeds_distinct_ids(
                seed_a in arb_seed(),
                seed_b in arb_seed(),
                name in arb_name(),
            ) {
                prop_assume!(seed_a != seed_b);
                let one = Swarm::new(seed_a, name.clone(), SwarmConfig::loopback()).to_string();
                let two = Swarm::new(seed_b, name, SwarmConfig::loopback()).to_string();
                prop_assert_ne!(one, two);
            }
        }
    }
}
