//! The swarm identifier, at two levels:
//!
//! - [`SwarmId`] — the validated `ahs…` *string* (shallow: prefix +
//!   length + Base58 charset). Cheap boundary check at the CLI / IPC
//!   edge. Code: [`id`].
//! - [`Swarm`] — the *decoded* structure (mode + name + 32-byte seed),
//!   with the Base58Check codec (this file). `SwarmId` is what flows
//!   through the wire/CLI; `Swarm` is what `setup_swarm` derives identity
//!   from.
//!
//! Also home to [`SwarmName`] ([`name`]), [`SwarmMode`] + the relay rule
//! ([`mode`]), and the lookup allowlist / `--advertise` selection
//! ([`lookup`]).

use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use iroh::EndpointId;
use sha2::{Digest, Sha256};

use super::crypto;

mod id;
mod lookup;
mod mode;
mod name;

pub use id::{SwarmId, SwarmIdError};
pub(crate) use lookup::{
    DEFAULT_DIRECTORY, DirectorySelection, LookupOpts, LookupSet, RelayChoice, RelaySelection,
    resolve_lookups, validate_advertise,
};
pub(crate) use mode::{SwarmMode, parse_relay_ladder, resolve_relay};
pub(crate) use name::SwarmName;

const PREFIX: &str = "ahs";

/// Wire version. v2 replaced the creator `EndpointId` with a random
/// `seed` (creator-independent rendezvous). No v1 compatibility — the
/// project is pre-release; old `ahs…` ids are rejected by the version
/// check below.
const VERSION: u8 = 2;
const SEED_LEN: usize = 32;
/// Wire bound for the encoded name in bytes. `SwarmName::new` caps the
/// name at `ident::MAX_CHARS` scalar values; each is at most 4 UTF-8
/// bytes, so the encoded form fits this many bytes (and inside the
/// 1-byte length field).
const NAME_MAX_BYTES: usize = super::ident::MAX_CHARS * 4;

/// A swarm identifier — Base58Check payload with an `ahs` prefix.
///
/// The token carries only the random `seed`; **no peer address is
/// ever stored**. The gossip topic, the well-known rendezvous identity
/// (every joiner's bootstrap target), and the private-mode loopback
/// port are all derived from `seed` in memory, so the swarm is
/// creator-independent and survives the creator's death.
///
/// Wire format (little-endian):
///   [1 byte version]
///   [1 byte mode]
///   [32 bytes seed]
///   [1 byte name length in bytes, 1..=128]
///   [N bytes name (UTF-8, <=32 scalars, charset enforced by `SwarmName`)]
#[derive(Debug, Clone)]
pub(crate) struct Swarm {
    pub mode: SwarmMode,
    pub name: SwarmName,
    seed: [u8; SEED_LEN],
}

impl Swarm {
    pub(crate) fn new(mode: SwarmMode, seed: [u8; SEED_LEN], name: SwarmName) -> Self {
        Swarm { mode, name, seed }
    }

    pub(crate) fn seed(&self) -> &[u8; SEED_LEN] {
        &self.seed
    }

    /// Well-known rendezvous `EndpointId`, derived from `seed`. Every
    /// joiner computes this locally and bootstraps gossip from it; it
    /// is co-hosted by members rather than pinned to the creator.
    pub(crate) fn rendezvous_id(&self) -> EndpointId {
        crypto::rendezvous_id(&self.seed)
    }

    /// Deterministic loopback port *ladder* for private swarms (no
    /// pkarr/DNS to resolve `rendezvous_id`). Preference order; a
    /// beacon binds the first free rung, joiners try all rungs.
    pub(crate) fn rendezvous_ports(&self) -> [u16; crypto::RENDEZVOUS_LADDER] {
        crypto::rendezvous_ports(&self.seed)
    }

    fn encode_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(3 + SEED_LEN + self.name.as_bytes().len());
        buf.push(VERSION);
        buf.push(self.mode.to_byte());
        buf.extend_from_slice(&self.seed);
        // SwarmName guarantees 1..=128 UTF-8 bytes, so a 1-byte length is safe.
        buf.push(self.name.len_u8());
        buf.extend_from_slice(self.name.as_bytes());
        buf
    }

    fn decode_bytes(bytes: &[u8]) -> Result<Self> {
        // [version][mode][32 seed][name_len] => 35 bytes minimum.
        if bytes.len() < 3 + SEED_LEN {
            bail!("Swarm identifier too short");
        }
        let version = bytes[0];
        if version != VERSION {
            bail!("Unsupported swarm version: {version}");
        }
        let mode = SwarmMode::from_byte(bytes[1])?;

        let mut seed = [0u8; SEED_LEN];
        seed.copy_from_slice(&bytes[2..2 + SEED_LEN]);

        let name_len_pos = 2 + SEED_LEN;
        let name_len = bytes[name_len_pos] as usize;
        if name_len == 0 || name_len > NAME_MAX_BYTES {
            bail!("Invalid swarm name length: {name_len}");
        }
        let name_start = name_len_pos + 1;
        let name_end = name_start + name_len;
        if name_end > bytes.len() {
            bail!("Truncated in swarm identifier");
        }
        let name_str = std::str::from_utf8(&bytes[name_start..name_end])
            .context("Invalid swarm name UTF-8")?;
        let name = SwarmName::new(name_str).context("Invalid swarm name")?;

        if name_end != bytes.len() {
            bail!("Trailing bytes in swarm identifier");
        }

        Ok(Swarm { mode, name, seed })
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
            .context("Invalid swarm prefix: expected 'ahs'")?;
        let bytes = base58check_decode(payload)?;
        Self::decode_bytes(&bytes)
    }
}

#[cfg(test)]
mod swarm_tests {
    use super::{SEED_LEN, Swarm, SwarmMode, SwarmName};

    fn dummy_seed() -> [u8; SEED_LEN] {
        [7u8; SEED_LEN]
    }

    fn dummy_name() -> SwarmName {
        SwarmName::new("test").unwrap()
    }

    #[test]
    fn round_trip_private() {
        let swarm = Swarm::new(SwarmMode::Private, dummy_seed(), dummy_name());
        let encoded = swarm.to_string();
        assert!(encoded.starts_with("ahs"));
        let decoded: Swarm = encoded.parse().unwrap();
        assert_eq!(decoded.mode, swarm.mode);
        assert_eq!(decoded.seed(), swarm.seed());
        assert_eq!(decoded.name, swarm.name);
    }

    #[test]
    fn round_trip_public() {
        let swarm = Swarm::new(SwarmMode::Public, [0xABu8; SEED_LEN], dummy_name());
        let decoded: Swarm = swarm.to_string().parse().unwrap();
        assert_eq!(decoded.mode, SwarmMode::Public);
        assert_eq!(decoded.seed(), &[0xABu8; SEED_LEN]);
        assert_eq!(decoded.name, swarm.name);
    }

    #[test]
    fn seed_drives_rendezvous_identity() {
        let swarm = Swarm::new(SwarmMode::Public, dummy_seed(), dummy_name());
        let decoded: Swarm = swarm.to_string().parse().unwrap();
        // Token round-trip preserves the derived rendezvous identity.
        assert_eq!(decoded.rendezvous_id(), swarm.rendezvous_id());
        assert_eq!(decoded.rendezvous_ports(), swarm.rendezvous_ports());
    }

    #[test]
    fn different_seeds_yield_different_ids() {
        let one = Swarm::new(SwarmMode::Private, [1u8; SEED_LEN], dummy_name()).to_string();
        let two = Swarm::new(SwarmMode::Private, [2u8; SEED_LEN], dummy_name()).to_string();
        assert_ne!(one, two);
    }

    #[test]
    fn invalid_prefix_rejected() {
        let swarm = Swarm::new(SwarmMode::Private, dummy_seed(), dummy_name());
        let encoded = swarm.to_string();
        let bad = format!("xxx{}", &encoded[3..]);
        assert!(bad.parse::<Swarm>().is_err());
    }

    #[test]
    fn non_ahs_prefix_rejected() {
        let swarm = Swarm::new(SwarmMode::Private, dummy_seed(), dummy_name());
        let encoded = swarm.to_string();
        for bad_prefix in ["sw1", "xyz", "AHS"] {
            let bad = format!("{}{}", bad_prefix, &encoded[3..]);
            assert!(
                bad.parse::<Swarm>().is_err(),
                "expected reject for prefix {bad_prefix}",
            );
        }
    }

    #[test]
    fn invalid_checksum_rejected() {
        let swarm = Swarm::new(SwarmMode::Private, dummy_seed(), dummy_name());
        let encoded = swarm.to_string();
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
    fn wrong_version_rejected() {
        let swarm = Swarm::new(SwarmMode::Private, dummy_seed(), dummy_name());
        let mut bytes = swarm.encode_bytes();
        bytes[0] = 1; // the now-unsupported v1
        assert!(Swarm::decode_bytes(&bytes).is_err());
    }

    #[test]
    fn swarm_name_validates_length() {
        assert!(SwarmName::new("").is_err());
        assert!(SwarmName::new("a").is_ok());
        assert!(SwarmName::new("a".repeat(32)).is_ok()); // 32 chars
        assert!(SwarmName::new("a".repeat(33)).is_err()); // 33 chars
        // Cap counts characters, not bytes: 32 multibyte chars are fine.
        assert!(SwarmName::new("あ".repeat(32)).is_ok());
        assert!(SwarmName::new("あ".repeat(33)).is_err());
    }

    #[test]
    fn swarm_name_validates_charset() {
        assert!(SwarmName::new("ok-name_1").is_ok());
        assert!(SwarmName::new("CamelCase").is_ok()); // uppercase
        assert!(SwarmName::new("1leading").is_ok()); // leading digit
        assert!(SwarmName::new("-leading").is_ok()); // leading dash
        assert!(SwarmName::new("emoji-🐝").is_ok());
        assert!(SwarmName::new("日本語").is_ok());
        assert!(SwarmName::new("dot.no").is_ok()); // dot is a fine symbol
        assert!(SwarmName::new("has space").is_err());
        assert!(SwarmName::new("slash/no").is_err());
        assert!(SwarmName::new("back\\no").is_err());
        assert!(SwarmName::new("nl\nno").is_err());
        assert!(SwarmName::new("nul\0no").is_err());
        assert!(SwarmName::new("rlo\u{202E}no").is_err()); // bidi override
        assert!(SwarmName::new("a<b").is_err()); // reserved for <nick>
        assert!(SwarmName::new("a>b").is_err()); // reserved for <nick>
        assert!(SwarmName::new("a#b").is_err()); // reserved for #swarm
    }

    #[test]
    fn swarm_name_random_validates() {
        for _ in 0..20 {
            let name = SwarmName::random();
            SwarmName::new(name.as_str()).expect("random must round-trip");
        }
    }

    #[test]
    fn swarm_id_round_trips_unicode_name() {
        let name = SwarmName::new("café-日本-🐝").unwrap();
        let swarm = Swarm::new(SwarmMode::Public, dummy_seed(), name.clone());
        let decoded: Swarm = swarm.to_string().parse().expect("decode failed");
        assert_eq!(decoded.name, name);
    }

    #[test]
    fn swarm_id_round_trips_max_byte_name() {
        // 32 four-byte scalars = 128 bytes = the most the 1-byte name
        // length field can carry; exercises the encode/decode upper edge.
        let name = SwarmName::new("🐝".repeat(32)).unwrap();
        assert_eq!(name.as_bytes().len(), 128);
        let swarm = Swarm::new(SwarmMode::Private, dummy_seed(), name.clone());
        let decoded: Swarm = swarm.to_string().parse().expect("decode failed");
        assert_eq!(decoded.name, name);
    }

    mod prop {
        use proptest::{
            array::uniform32, prelude::any, prop_assert, prop_assert_eq, prop_assert_ne,
            prop_assume, proptest, strategy::Strategy,
        };

        use super::{SEED_LEN, Swarm, SwarmMode, SwarmName};

        fn arb_seed() -> impl Strategy<Value = [u8; SEED_LEN]> {
            uniform32(0u8..)
        }

        fn arb_name() -> impl Strategy<Value = SwarmName> {
            "[a-z][a-z0-9_-]{0,31}".prop_map(|raw| SwarmName::new(raw).unwrap())
        }

        proptest! {
            #[test]
            fn prop_round_trip(
                seed in arb_seed(),
                name in arb_name(),
                mode in any::<bool>(),
            ) {
                let mode = if mode { SwarmMode::Public } else { SwarmMode::Private };
                let swarm = Swarm::new(mode, seed, name.clone());
                let encoded = swarm.to_string();
                let decoded: Swarm = encoded.parse().expect("decode failed");

                prop_assert_eq!(decoded.mode, swarm.mode);
                prop_assert_eq!(decoded.seed(), swarm.seed());
                prop_assert_eq!(decoded.name, swarm.name);
            }

            #[test]
            fn prop_prefix(seed in arb_seed(), name in arb_name()) {
                let swarm = Swarm::new(SwarmMode::Private, seed, name);
                prop_assert!(swarm.to_string().starts_with("ahs"));
            }

            #[test]
            fn prop_deterministic(
                seed in arb_seed(),
                name in arb_name(),
                mode in any::<bool>(),
            ) {
                let mode = if mode { SwarmMode::Public } else { SwarmMode::Private };
                let swarm = Swarm::new(mode, seed, name);
                prop_assert_eq!(swarm.to_string(), swarm.to_string());
            }

            #[test]
            fn prop_distinct_seeds_distinct_ids(
                seed_a in arb_seed(),
                seed_b in arb_seed(),
                name in arb_name(),
            ) {
                prop_assume!(seed_a != seed_b);
                let one = Swarm::new(SwarmMode::Public, seed_a, name.clone()).to_string();
                let two = Swarm::new(SwarmMode::Public, seed_b, name).to_string();
                prop_assert_ne!(one, two);
            }
        }
    }
}
