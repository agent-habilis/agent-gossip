//! The swarm identifier, at two levels:
//!
//! - [`SwarmId`] — the validated `ahs…` *string* (shallow: prefix +
//!   length + Base58 charset). Cheap boundary check at the CLI / IPC
//!   edge.
//! - [`Swarm`] — the *decoded* structure (mode + name + 32-byte seed),
//!   with the Base58Check codec. `SwarmId` is what flows through the
//!   wire/CLI; `Swarm` is what `setup_swarm` derives identity from.
//!
//! Also home to [`SwarmName`], [`SwarmMode`], and the "a relay is only
//! meaningful on the public network" rule shared by every create path.

use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use iroh::{EndpointId, RelayUrl};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::crypto;

const PREFIX: &str = "ahs";

// ── SwarmId (validated string form) ──────────────────────────────

const MIN_LEN: usize = 7;
const MAX_LEN: usize = 512;

/// A swarm identifier — the encoded `ahs...` Base58Check string.
///
/// Validation is shallow: prefix `ahs`, length 7..=512, Base58
/// charset (`[1-9A-HJ-NP-Za-km-z]`). Full structural decoding lives
/// in `Swarm::from_str`; the newtype rejects obvious typos at the
/// CLI / IPC boundary without paying the decode cost on every flow.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SwarmId(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwarmIdError {
    MissingPrefix,
    Length(usize),
    Charset(String),
}

impl fmt::Display for SwarmIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SwarmIdError::MissingPrefix => write!(formatter, "swarm id must start with '{PREFIX}'"),
            SwarmIdError::Length(len) => {
                write!(
                    formatter,
                    "swarm id must be {MIN_LEN}..={MAX_LEN} chars, got {len}"
                )
            }
            SwarmIdError::Charset(value) => {
                write!(formatter, "swarm id has invalid Base58 char(s): {value:?}")
            }
        }
    }
}

impl std::error::Error for SwarmIdError {}

fn is_base58_char(ch: char) -> bool {
    matches!(ch,
        '1'..='9'
        | 'A'..='H' | 'J'..='N' | 'P'..='Z'
        | 'a'..='k' | 'm'..='z'
    )
}

impl SwarmId {
    pub(crate) fn new(value: impl Into<String>) -> Result<Self, SwarmIdError> {
        let value = value.into();
        if !value.starts_with(PREFIX) {
            return Err(SwarmIdError::MissingPrefix);
        }
        if value.len() < MIN_LEN || value.len() > MAX_LEN {
            return Err(SwarmIdError::Length(value.len()));
        }
        let payload = &value[PREFIX.len()..];
        if !payload.chars().all(is_base58_char) {
            return Err(SwarmIdError::Charset(value));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SwarmId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for SwarmId {
    type Err = SwarmIdError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::new(text)
    }
}

impl AsRef<str> for SwarmId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for SwarmId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
impl From<&str> for SwarmId {
    fn from(text: &str) -> Self {
        Self::new(text).expect("invalid swarm id in test fixture")
    }
}

#[cfg(test)]
mod swarm_id_tests {
    use super::{SwarmId, SwarmIdError};

    #[test]
    fn new_accepts_well_formed_ahs() {
        SwarmId::new("ahsAbCdEf1234").unwrap();
    }

    #[test]
    fn new_rejects_missing_prefix() {
        assert!(matches!(
            SwarmId::new("noprefix12345"),
            Err(SwarmIdError::MissingPrefix)
        ));
    }

    #[test]
    fn new_rejects_too_short() {
        assert!(matches!(SwarmId::new("ahsa"), Err(SwarmIdError::Length(_))));
    }

    #[test]
    fn new_rejects_invalid_base58_chars() {
        // `0`, `O`, `I`, `l` are not in the Base58 alphabet.
        assert!(matches!(
            SwarmId::new("ahsAbCdEf0xyz"),
            Err(SwarmIdError::Charset(_))
        ));
    }

    #[test]
    fn serde_transparent_round_trip() {
        let id = SwarmId::from("ahsAbCdEf1234");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"ahsAbCdEf1234\"");
        let parsed: SwarmId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, id);
    }
}

// ── SwarmName ────────────────────────────────────────────────────

/// Wire version. v2 replaced the creator `EndpointId` with a random
/// `seed` (creator-independent rendezvous). No v1 compatibility — the
/// project is pre-release; old `ahs…` ids are rejected by the version
/// check below.
const VERSION: u8 = 2;
const SEED_LEN: usize = 32;
const NAME_MAX_LEN: usize = 32;

/// A human-readable swarm label, bound cryptographically into the topic id.
///
/// Same rules as `Nickname`: 1..=32 chars, charset `[a-z0-9_-]`, must
/// start with a lowercase letter. The newtype is the single validation
/// point — every construction path goes through `new`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct SwarmName(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NameError {
    Length(usize),
    Charset(String),
    LeadingChar(char),
}

impl fmt::Display for NameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NameError::Length(len) => {
                write!(
                    formatter,
                    "swarm name must be 1..={NAME_MAX_LEN} chars, got {len}"
                )
            }
            NameError::LeadingChar(ch) => {
                write!(
                    formatter,
                    "swarm name must start with a lowercase letter, got {ch:?}"
                )
            }
            NameError::Charset(value) => {
                write!(
                    formatter,
                    "swarm name must contain only [a-z0-9_-], got {value:?}"
                )
            }
        }
    }
}

impl std::error::Error for NameError {}

impl SwarmName {
    pub(crate) fn new(value: impl Into<String>) -> std::result::Result<Self, NameError> {
        let value = value.into();
        if value.is_empty() || value.len() > NAME_MAX_LEN {
            return Err(NameError::Length(value.len()));
        }
        let Some(first) = value.chars().next() else {
            return Err(NameError::Length(value.len()));
        };
        if !first.is_ascii_lowercase() {
            return Err(NameError::LeadingChar(first));
        }
        if !value
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
        {
            return Err(NameError::Charset(value));
        }
        Ok(Self(value))
    }

    /// Generate a random `word-word` swarm name from the curated
    /// wordlist — the same generator nicknames use.
    pub(crate) fn random() -> Self {
        Self::new(super::wordlist::random_pair())
            .expect("wordlist pair is always a valid swarm name")
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    /// Byte length as a `u8`. `new` bounds the name to `NAME_MAX_LEN`
    /// (<= 32), so this never truncates.
    pub(crate) fn len_u8(&self) -> u8 {
        u8::try_from(self.0.len()).expect("SwarmName is <= 32 bytes")
    }
}

impl fmt::Display for SwarmName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for SwarmName {
    type Err = NameError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Self::new(s)
    }
}

// ── SwarmMode + relay rule ───────────────────────────────────────

/// Network mode encoded in swarm identifiers.
///
/// - `Private`: loopback only (same machine).
/// - `Public`: open-internet — rendezvous via mDNS/DHT, beacon on a
///   pinned (or `--relay`) relay.
///
/// Wire bytes: `Private=0`, `Public=1`. Unknown bytes are rejected so
/// existing `ahs…` IDs remain the only valid ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SwarmMode {
    Private,
    Public,
}

impl SwarmMode {
    fn to_byte(self) -> u8 {
        match self {
            SwarmMode::Private => 0,
            SwarmMode::Public => 1,
        }
    }

    fn from_byte(value: u8) -> Result<Self> {
        match value {
            0 => Ok(SwarmMode::Private),
            1 => Ok(SwarmMode::Public),
            _ => bail!("Invalid swarm mode byte: {value}"),
        }
    }

    /// Parse the textual network name accepted by the MCP
    /// `create_swarm` tool. The CLI uses a `--public` bool; the
    /// embed facade uses a `bool`.
    pub(crate) fn from_network_name(name: &str) -> Result<Self> {
        match name {
            "private" => Ok(SwarmMode::Private),
            "public" => Ok(SwarmMode::Public),
            other => bail!("unknown network mode: {other} (expected 'private' or 'public')"),
        }
    }

    /// Inverse of [`from_network_name`].
    pub(crate) fn network_name(self) -> &'static str {
        match self {
            SwarmMode::Private => "private",
            SwarmMode::Public => "public",
        }
    }
}

/// The "a relay is only meaningful on the public network" rule for
/// the relay-as-string paths (MCP / embed). For flag-shaped inputs
/// the CLI uses [`validate_discovery`], which generalises this; for
/// callers that hand a relay as a string, prefer [`resolve_relay`]
/// (guard + parse).
pub(crate) fn require_relay_public(mode: SwarmMode, has_relay: bool) -> Result<()> {
    if has_relay && mode != SwarmMode::Public {
        bail!("a relay requires the public network");
    }
    Ok(())
}

/// Enforce [`require_relay_public`] then parse the relay string into a
/// `RelayUrl`. Used by the create paths that take a relay as text
/// (MCP tool args, embed `CreateConfig`).
pub(crate) fn resolve_relay(mode: SwarmMode, relay: Option<&str>) -> Result<Option<RelayUrl>> {
    require_relay_public(mode, relay.is_some())?;
    relay
        .map(|raw| {
            raw.parse::<RelayUrl>()
                .map_err(|error| anyhow::anyhow!("invalid relay URL: {error}"))
        })
        .transpose()
}

// ── discovery (address-lookup + relay) ───────────────────────────

/// The resolved discovery + connectivity config the endpoint builder
/// applies. `mdns`/`dht` are the enabled iroh address-lookups (both
/// resolve the same seed-derived `rendezvous_id`). `relay` is the
/// connectivity relay: `None` ⇒ the single pinned default (on
/// `public`) or no relay (on `private`); `Some(url)` ⇒ a custom
/// relay. The relay is never explicitly disabled — it is a URL, not a
/// toggle.
#[derive(Debug, Clone)]
pub(crate) struct DiscoveryOpts {
    pub mdns: bool,
    pub dht: bool,
    pub relay: Option<RelayUrl>,
}

impl DiscoveryOpts {
    /// The default behaviour, kept stable for the in-process
    /// embed/MCP sessions: `private` ⇒ everything off (loopback
    /// ladder); `public` ⇒ all lookups (mdns + dht) + the pinned (or
    /// custom) relay. Expressed via [`resolve_discovery`] so the
    /// resolved-shape logic has one home.
    pub(crate) fn legacy(mode: SwarmMode, relay: Option<RelayUrl>) -> Self {
        // Empty set ⇒ resolver enables all lookups on `public` and
        // forces all-off on `private`; both are always mode-valid
        // (upstream `resolve_relay` rejects a private relay).
        resolve_discovery(mode, LookupSet::default(), relay)
            .expect("legacy DiscoveryOpts inputs are always mode-valid")
    }
}

/// The selected address-lookups (presence allowlist): the lookup
/// kinds a member can enable. Relay is separate (connectivity,
/// controlled by `--relay`).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LookupSet {
    pub mdns: bool,
    pub dht: bool,
}

impl LookupSet {
    fn any(self) -> bool {
        self.mdns || self.dht
    }
}

/// One network-compatibility guard (generalises `require_relay_public`
/// to every discovery flag). `private` is loopback-only, so any
/// `--mdns`/`--dht` or explicit `--relay` is rejected, naming them all
/// in a single message — never a silent no-op.
pub(crate) fn validate_discovery(
    mode: SwarmMode,
    lookups: LookupSet,
    relay_present: bool,
) -> Result<()> {
    if mode == SwarmMode::Public {
        return Ok(());
    }
    let mut offending = Vec::new();
    if lookups.mdns {
        offending.push("--mdns");
    }
    if lookups.dht {
        offending.push("--dht");
    }
    if relay_present {
        offending.push("--relay");
    }
    if offending.is_empty() {
        Ok(())
    } else {
        bail!(
            "{} require the public network; pass --public",
            offending.join(", ")
        );
    }
}

/// Resolve the lookup set + relay choice against the network mode
/// into the effective [`DiscoveryOpts`]. On `public`: naming **no**
/// lookup flag enables both (mdns + dht); naming **any**
/// (`--mdns`/`--dht`) uses *only* those passed. `relay` is the parsed
/// `--relay` (`None` ⇒ the pinned default). Errors if any
/// `--mdns`/`--dht`/`--relay` is given with `private`.
pub(crate) fn resolve_discovery(
    mode: SwarmMode,
    lookups: LookupSet,
    relay: Option<RelayUrl>,
) -> Result<DiscoveryOpts> {
    validate_discovery(mode, lookups, relay.is_some())?;
    match mode {
        SwarmMode::Private => Ok(DiscoveryOpts {
            mdns: false,
            dht: false,
            relay: None,
        }),
        SwarmMode::Public => {
            // Any lookup flag ⇒ use *only* those passed; none ⇒ all.
            let (mdns, dht) = if lookups.any() {
                (lookups.mdns, lookups.dht)
            } else {
                (true, true)
            };
            Ok(DiscoveryOpts { mdns, dht, relay })
        }
    }
}

#[cfg(test)]
mod discovery_tests {
    use super::{LookupSet, SwarmMode, resolve_discovery, validate_discovery};

    fn set(mdns: bool, dht: bool) -> LookupSet {
        LookupSet { mdns, dht }
    }

    fn url() -> iroh::RelayUrl {
        "https://relay.example".parse().unwrap()
    }

    #[test]
    fn public_no_flags_enables_all_lookups() {
        let opts = resolve_discovery(SwarmMode::Public, LookupSet::default(), None).unwrap();
        assert!(opts.mdns && opts.dht);
        assert!(opts.relay.is_none(), "no --relay ⇒ pinned default");
    }

    #[test]
    fn public_any_lookup_flag_uses_only_those_passed() {
        // Naming any lookup flag ⇒ exactly those passed.
        let only_mdns = resolve_discovery(SwarmMode::Public, set(true, false), None).unwrap();
        assert!(only_mdns.mdns && !only_mdns.dht);
        let only_dht = resolve_discovery(SwarmMode::Public, set(false, true), None).unwrap();
        assert!(!only_dht.mdns && only_dht.dht);
    }

    #[test]
    fn public_custom_relay_passes_through() {
        let opts = resolve_discovery(SwarmMode::Public, LookupSet::default(), Some(url())).unwrap();
        assert_eq!(opts.relay, Some(url()));
    }

    #[test]
    fn private_no_flags_is_all_off() {
        let opts = resolve_discovery(SwarmMode::Private, LookupSet::default(), None).unwrap();
        assert!(!opts.mdns && !opts.dht);
        assert!(opts.relay.is_none());
    }

    #[test]
    fn private_with_any_lookup_or_relay_flag_is_rejected() {
        let cases: [(LookupSet, Option<iroh::RelayUrl>); 3] = [
            (set(true, false), None),
            (set(false, true), None),
            (LookupSet::default(), Some(url())),
        ];
        for (lookups, relay) in cases {
            let via_resolve = resolve_discovery(SwarmMode::Private, lookups, relay.clone());
            assert!(
                via_resolve.is_err(),
                "resolve must reject: {lookups:?} {relay:?}"
            );
            let error =
                validate_discovery(SwarmMode::Private, lookups, relay.is_some()).unwrap_err();
            assert!(error.to_string().contains("--public"), "got: {error}");
        }
    }
}

// ── Swarm (decoded structure + Base58Check codec) ────────────────

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
///   [1 byte name length, 1..=32]
///   [N bytes name (ASCII, charset enforced by `SwarmName`)]
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
        let mut buf = Vec::with_capacity(3 + SEED_LEN + self.name.0.len());
        buf.push(VERSION);
        buf.push(self.mode.to_byte());
        buf.extend_from_slice(&self.seed);
        // SwarmName guarantees 1..=32 ASCII bytes, so a 1-byte length is safe.
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
        if name_len == 0 || name_len > NAME_MAX_LEN {
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
    use super::*;

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
        assert!(SwarmName::new("a".repeat(32)).is_ok()); // 32
        assert!(SwarmName::new("a".repeat(33)).is_err()); // 33
    }

    #[test]
    fn swarm_name_validates_charset() {
        assert!(SwarmName::new("ok-name_1").is_ok());
        assert!(SwarmName::new("CamelCase").is_err()); // uppercase
        assert!(SwarmName::new("1leading").is_err()); // leading digit
        assert!(SwarmName::new("-leading").is_err()); // leading dash
        assert!(SwarmName::new("has space").is_err());
        assert!(SwarmName::new("emoji-🐝").is_err());
        assert!(SwarmName::new("slash/no").is_err());
        assert!(SwarmName::new("dot.no").is_err());
    }

    #[test]
    fn swarm_name_random_validates() {
        for _ in 0..20 {
            let name = SwarmName::random();
            SwarmName::new(name.as_str()).expect("random must round-trip");
        }
    }

    mod prop {
        use super::*;
        use proptest::array::uniform32;
        use proptest::prelude::*;

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
