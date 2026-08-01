//! The mesh-wide config carried in the mesh id — the lookup
//! allowlist (`mdns`/`dht`/`relay`) — plus its byte codec and the
//! `--advertise` directory selection. A mesh's network reach is fully
//! described by its lookups: no lookups means loopback-only; any lookup
//! means reachable across machines.

use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use iroh::RelayUrl;

use super::MeshName;
use crate::protocol::crypto::PASSWORD_VERIFIER_LEN;

/// The connectivity relay. `Disabled` ⇒ no relay at all
/// (`RelayMode::Disabled`); `Pinned` ⇒ the lookup-layer pinned default
/// *ladder* (the n0 prod set); `Custom` ⇒ an operator-supplied **ordered
/// ladder** (`--relay a,b,c`). Relay is an allowlist member like
/// mdns/dht, not an always-on URL — the lookup layer turns
/// `Pinned`/`Custom` into an ordered relay ladder, and the beacon homes
/// on the first reachable rung (see `lookup::relay_ladder` /
/// `lookup::select_bootstrap_rung`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayChoice {
    Disabled,
    Pinned,
    Custom(Vec<RelayUrl>),
}

/// The lookup allowlist baked into the mesh id. `mdns`/`dht` are the
/// enabled iroh address-lookups (both resolve the same seed-derived
/// `rendezvous_id`); `relay` is the connectivity relay (see
/// [`RelayChoice`]). An all-off set is a loopback-only mesh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupOpts {
    pub mdns: bool,
    pub dht: bool,
    pub relay: RelayChoice,
}

/// Wire ceiling on a custom relay ladder, so a forged id can't blow up
/// allocation. Far above any real ladder.
const MAX_RELAY_LADDER: usize = 16;
/// Wire ceiling on a single relay URL's byte length.
const MAX_RELAY_URL_BYTES: usize = 512;

impl LookupOpts {
    /// Loopback-only: no address-lookups, no relay (the seed-derived
    /// port ladder bootstraps everything on one machine).
    #[must_use]
    pub fn loopback() -> Self {
        LookupOpts {
            mdns: false,
            dht: false,
            relay: RelayChoice::Disabled,
        }
    }

    /// The all-on default for a mesh reachable across machines: both
    /// address-lookups plus the pinned default relay ladder.
    #[must_use]
    pub fn public_preset() -> Self {
        LookupOpts {
            mdns: true,
            dht: true,
            relay: RelayChoice::Pinned,
        }
    }

    /// True when nothing reaches off-machine — the mesh is loopback-only.
    #[must_use]
    pub fn is_loopback(&self) -> bool {
        !self.mdns && !self.dht && self.relay == RelayChoice::Disabled
    }

    /// Human/JSON label for the mesh's reach. Derived from the lookups —
    /// there is no stored network mode.
    pub(crate) fn network_label(&self) -> &'static str {
        if self.is_loopback() {
            "private"
        } else {
            "public"
        }
    }

    /// Append the canonical wire encoding to `buf`:
    /// `[flags u8][if custom: [count u8] ([len u16 LE] url)*]`.
    /// # Panics
    /// Panics if an internal invariant is violated.
    pub fn encode_into(&self, buf: &mut Vec<u8>) {
        let mut flags: u8 = 0;
        if self.mdns {
            flags |= 0b0001;
        }
        if self.dht {
            flags |= 0b0010;
        }
        match &self.relay {
            RelayChoice::Disabled => {}
            RelayChoice::Pinned => flags |= 0b0100,
            RelayChoice::Custom(_) => flags |= 0b0100 | 0b1000,
        }
        buf.push(flags);
        if let RelayChoice::Custom(ladder) = &self.relay {
            // The ladder is created locally and bounded by the CLI / library API,
            // so this cast and the lengths below always fit.
            buf.push(u8::try_from(ladder.len()).expect("relay ladder bounded by MAX_RELAY_LADDER"));
            for url in ladder {
                let text = url.to_string();
                let len =
                    u16::try_from(text.len()).expect("relay URL bounded by MAX_RELAY_URL_BYTES");
                buf.extend_from_slice(&len.to_le_bytes());
                buf.extend_from_slice(text.as_bytes());
            }
        }
    }

    /// Decode from a cursor over the config region, advancing `pos`.
    /// # Errors
    /// Returns an error if the inputs are invalid or the operation fails.
    pub fn decode_from(bytes: &[u8], pos: &mut usize) -> Result<Self> {
        let flags = *bytes.get(*pos).context("truncated lookup flags")?;
        *pos += 1;
        let mdns = flags & 0b0001 != 0;
        let dht = flags & 0b0010 != 0;
        let relay_enabled = flags & 0b0100 != 0;
        let relay_custom = flags & 0b1000 != 0;
        if relay_custom && !relay_enabled {
            bail!("custom-relay bit set without relay-enabled bit");
        }
        let relay = if !relay_enabled {
            RelayChoice::Disabled
        } else if !relay_custom {
            RelayChoice::Pinned
        } else {
            let count = *bytes.get(*pos).context("truncated relay ladder count")? as usize;
            *pos += 1;
            if count == 0 {
                bail!("custom relay ladder is empty");
            }
            if count > MAX_RELAY_LADDER {
                bail!("relay ladder too long: {count}");
            }
            RelayChoice::Custom(decode_relay_ladder(bytes, pos, count)?)
        };
        Ok(LookupOpts { mdns, dht, relay })
    }
}

/// Decode `count` length-prefixed relay URLs from the cursor, advancing `pos`.
fn decode_relay_ladder(bytes: &[u8], pos: &mut usize, count: usize) -> Result<Vec<RelayUrl>> {
    let mut ladder = Vec::with_capacity(count);
    for _ in 0..count {
        let len = read_u16(bytes, pos).context("truncated relay URL length")? as usize;
        if len > MAX_RELAY_URL_BYTES {
            bail!("relay URL too long: {len}");
        }
        let end = pos.checked_add(len).context("relay URL length overflow")?;
        let raw = bytes.get(*pos..end).context("truncated relay URL")?;
        *pos = end;
        let text = std::str::from_utf8(raw).context("relay URL is not UTF-8")?;
        ladder.push(text.parse::<RelayUrl>().context("invalid relay URL")?);
    }
    Ok(ladder)
}

pub(super) fn read_u16(bytes: &[u8], pos: &mut usize) -> Result<u16> {
    let end = pos.checked_add(2).context("u16 length overflow")?;
    let slice = bytes.get(*pos..end).context("truncated u16")?;
    *pos = end;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

/// Feature byte appended after the lookups when a mesh carries a password
/// and/or is invite-only. Appended — not a spare lookup-flags bit — because
/// old binaries ignore unknown flag bits (they would silently decode a
/// featured id and sit in an empty topic) but hard-error on trailing config
/// bytes. One feature byte gates both features; their fields follow in a fixed
/// order (password verifier, then issuer pubkey).
const FEATURE_PASSWORD: u8 = 0b0001;

/// Feature bit marking an invite-only mesh: joining needs a creator-minted
/// invite ticket, not the bare hash. The issuer public key (below) follows the
/// password verifier when both bits are set.
const FEATURE_INVITE_ONLY: u8 = 0b0010;

/// Byte length of the Ed25519 issuer public key an invite-only mesh carries.
const ISSUER_PUBKEY_LEN: usize = 32;

/// The mesh-wide configuration carried in the id and mixed into the
/// gossip topic, so every member that joins behaves identically. A
/// different config is a different mesh (different topic).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshConfig {
    pub lookups: LookupOpts,
    /// The password verifier — a one-way check value derived from the
    /// Argon2id-stretched password (`crypto::password_verifier`), never the
    /// password itself. `None` ⇒ passwordless. Carried in the id so `join`
    /// can verify a candidate password locally before any network.
    pub password: Option<[u8; PASSWORD_VERIFIER_LEN]>,
    /// The Ed25519 issuer public key of an **invite-only** mesh — the mint
    /// authority. `Some` ⇒ invite-only: the bare hash reaches nothing (the
    /// derivation secret lives only in creator-minted invites), and a redeemer
    /// verifies each invite's signature against this key. `None` ⇒ open join.
    pub issuer_pubkey: Option<[u8; ISSUER_PUBKEY_LEN]>,
}

impl MeshConfig {
    /// Default loopback-only config: no lookups. Test-only since the
    /// directory now builds its config from explicit lookups and `create`
    /// constructs `MeshConfig` directly.
    #[cfg(any(test, feature = "test-fixtures"))]
    #[must_use]
    pub fn loopback() -> Self {
        MeshConfig {
            lookups: LookupOpts::loopback(),
            password: None,
            issuer_pubkey: None,
        }
    }

    /// Default reachable-across-machines config: the all-on lookup preset.
    /// Test-only (see [`MeshConfig::loopback`]).
    #[cfg(any(test, feature = "test-fixtures"))]
    #[must_use]
    pub fn public_preset() -> Self {
        MeshConfig {
            lookups: LookupOpts::public_preset(),
            password: None,
            issuer_pubkey: None,
        }
    }

    /// Canonical wire bytes: `[lookups…][if password: feature-flags u8 ‖
    /// verifier]`. This exact byte string is what the id carries and what
    /// the topic derivation mixes in, so it must be deterministic — the
    /// feature byte is emitted only when nonzero (a passwordless config
    /// stays byte-for-byte what it was before features existed).
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(2);
        self.lookups.encode_into(&mut buf);
        let mut features = 0u8;
        if self.password.is_some() {
            features |= FEATURE_PASSWORD;
        }
        if self.issuer_pubkey.is_some() {
            features |= FEATURE_INVITE_ONLY;
        }
        if features != 0 {
            buf.push(features);
            // Fixed field order so the encoding is canonical: password verifier
            // then issuer pubkey, each present iff its bit is set.
            if let Some(verifier) = &self.password {
                buf.extend_from_slice(verifier);
            }
            if let Some(pubkey) = &self.issuer_pubkey {
                buf.extend_from_slice(pubkey);
            }
        }
        buf
    }

    /// Decode a config region, requiring it to consume `bytes` exactly
    /// (no trailing slack within the length-delimited region we were given).
    ///
    /// # Errors
    /// The bytes are malformed or do not decode to a valid config region.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut pos = 0;
        let lookups = LookupOpts::decode_from(bytes, &mut pos)?;
        let (password, issuer_pubkey) = if pos == bytes.len() {
            (None, None)
        } else {
            let features = bytes[pos];
            pos += 1;
            if features & !(FEATURE_PASSWORD | FEATURE_INVITE_ONLY) != 0 {
                bail!("unsupported mesh feature flags {features:#04x} — upgrade to a newer build");
            }
            if features == 0 {
                // A zero feature byte re-encodes without itself, silently
                // changing the topic-derivation bytes — reject the
                // non-canonical form outright.
                bail!("non-canonical mesh config: zero feature flags");
            }
            let password = if features & FEATURE_PASSWORD != 0 {
                let end = pos
                    .checked_add(PASSWORD_VERIFIER_LEN)
                    .context("password verifier length overflow")?;
                let raw = bytes.get(pos..end).context("truncated password verifier")?;
                pos = end;
                let mut verifier = [0u8; PASSWORD_VERIFIER_LEN];
                verifier.copy_from_slice(raw);
                Some(verifier)
            } else {
                None
            };
            let issuer_pubkey = if features & FEATURE_INVITE_ONLY != 0 {
                let end = pos
                    .checked_add(ISSUER_PUBKEY_LEN)
                    .context("issuer pubkey length overflow")?;
                let raw = bytes.get(pos..end).context("truncated issuer pubkey")?;
                pos = end;
                let mut pubkey = [0u8; ISSUER_PUBKEY_LEN];
                pubkey.copy_from_slice(raw);
                Some(pubkey)
            } else {
                None
            };
            (password, issuer_pubkey)
        };
        if pos != bytes.len() {
            bail!("trailing bytes in mesh config");
        }
        Ok(MeshConfig {
            lookups,
            password,
            issuer_pubkey,
        })
    }
}

/// Relay intent in a [`LookupSet`]: absent / default / custom. Resolved
/// into a `RelayChoice` by `resolve_lookups`. `Custom` carries the
/// ordered [`RelayLadder`] (iroh-free), so this enum is part of the public
/// library API surface.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RelaySelection {
    /// No relay (the CLI `--relay` flag absent).
    #[default]
    Unset,
    /// The pinned default n0 prod relay ladder (bare `--relay`).
    Default,
    /// A custom ordered ladder (`--relay a,b,c`).
    Custom(RelayLadder),
}

impl RelaySelection {
    fn is_set(&self) -> bool {
        !matches!(self, RelaySelection::Unset)
    }
}

impl FromStr for RelaySelection {
    type Err = RelayLadderError;

    /// Parse a `--relay` optional-value flag. The bare form resolves via the
    /// `"default"` `default_missing_value` (the same token the MCP / library API
    /// surface uses) to [`RelaySelection::Default`]; any other value is a
    /// custom ladder. `Unset` comes from the *absent* flag (`Option::None`),
    /// not from this parser.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text == "default" {
            Ok(RelaySelection::Default)
        } else {
            text.parse::<RelayLadder>().map(RelaySelection::Custom)
        }
    }
}

/// CLI `--advertise` intent: absent / bare / valued — the same
/// three-state optional-value shape as [`RelaySelection`]. `Unset` ⇒
/// the mesh is not listed in any directory; `Default` ⇒ the well-known
/// `global` directory; `Named` ⇒ a custom directory. The directory name is itself a
/// [`MeshName`] (same charset), since the directory derives its
/// mesh from it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DirectorySelection {
    #[default]
    Unset,
    Default,
    Named(MeshName),
}

/// The well-known default directory — used when `--advertise` is passed
/// bare (no value).
pub const DEFAULT_DIRECTORY: &str = "global";

impl DirectorySelection {
    /// Resolve a clap three-state `--advertise` optional-value flag
    /// (absent / bare / valued) — the one converter shared by every
    /// command that advertises (`create`, `pipe listen`, `file send`,
    /// `port listen`).
    /// Resolve a clap `--advertise` optional-value flag into a
    /// [`DirectorySelection`]. The bare form resolves via the `"global"`
    /// [`DEFAULT_DIRECTORY`] `default_missing_value`, so it arrives here as
    /// `Some(global)` — behaviorally identical to [`DirectorySelection::Default`]
    /// (both advertise into the well-known directory). `Unset` comes from the
    /// *absent* flag.
    #[must_use]
    pub fn from_flag(flag: Option<MeshName>) -> Self {
        match flag {
            None => DirectorySelection::Unset,
            Some(directory) => DirectorySelection::Named(directory),
        }
    }

    /// `true` when advertising is requested at all (bare or valued).
    pub(crate) fn is_set(&self) -> bool {
        !matches!(self, DirectorySelection::Unset)
    }

    /// The directory to advertise into, or `None` when not advertising.
    /// Bare ⇒ the [`DEFAULT_DIRECTORY`]; valued ⇒ the given name.
    #[must_use]
    /// # Panics
    /// Panics if an internal invariant is violated.
    pub fn directory(&self) -> Option<MeshName> {
        match self {
            DirectorySelection::Unset => None,
            DirectorySelection::Default => Some(
                MeshName::new(DEFAULT_DIRECTORY).expect("DEFAULT_DIRECTORY is a valid mesh name"),
            ),
            DirectorySelection::Named(name) => Some(name.clone()),
        }
    }
}

/// Advertise was requested on a loopback-only mesh. A directory listing
/// requires a mesh reachable across machines, so this is a hard error
/// (never a silent no-op). Typed so callers can classify it — the MCP
/// server maps it to `invalid_params`, the CLI to an `anyhow` bail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvertiseRequiresReachable;

impl fmt::Display for AdvertiseRequiresReachable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("advertise needs a reachable mesh; enable a lookup (e.g. public)")
    }
}

impl std::error::Error for AdvertiseRequiresReachable {}

/// `--advertise` lists the mesh in a public directory, so it requires a
/// mesh that is actually reachable across machines.
/// # Errors
/// Returns an error if the inputs are invalid or the operation fails.
pub fn validate_advertise(
    advertise: &DirectorySelection,
    lookups: &LookupOpts,
) -> Result<(), AdvertiseRequiresReachable> {
    if advertise.is_set()
        && lookups.is_loopback()
        && !crate::util::tuning::directory_private_for_test()
    {
        return Err(AdvertiseRequiresReachable);
    }
    Ok(())
}

/// The lookup flags the user selected on the CLI. `mdns`/`dht` are
/// address-lookups; `relay` is the connectivity/relay-direct rendezvous
/// path.
#[derive(Debug, Clone, Default)]
pub struct LookupSet {
    pub mdns: bool,
    pub dht: bool,
    pub relay: RelaySelection,
}

impl LookupSet {
    fn any(&self) -> bool {
        self.mdns || self.dht || self.relay.is_set()
    }
}

/// Resolve the CLI inputs into the effective [`LookupOpts`] baked into
/// the mesh id. Naming **any** lookup flag uses *only* those passed (so
/// `--mdns` alone is mDNS-only, relay/dht off); naming **none** but
/// passing `--public` enables the all-on preset; naming nothing at all is
/// a loopback-only mesh. `--relay` bare ⇒ pinned default, `--relay
/// <url>` ⇒ custom ladder.
#[must_use]
pub fn resolve_lookups(public: bool, lookups: LookupSet) -> LookupOpts {
    if lookups.any() {
        let relay = match lookups.relay {
            RelaySelection::Unset => RelayChoice::Disabled,
            RelaySelection::Default => RelayChoice::Pinned,
            RelaySelection::Custom(ladder) => RelayChoice::Custom(ladder.as_urls().to_vec()),
        };
        LookupOpts {
            mdns: lookups.mdns,
            dht: lookups.dht,
            relay,
        }
    } else if public {
        LookupOpts::public_preset()
    } else {
        LookupOpts::loopback()
    }
}

/// Parse a comma-separated, ordered relay **ladder** (`a,b,c`) — order
/// preserved (the beacon homes on the first reachable rung); an empty or
/// whitespace-only entry is a hard error so a typo never silently
/// shrinks the ladder. The single source of truth for `--relay` syntax,
/// shared by the CLI value-parser and `RelayLadder` (the MCP / library API
/// string path); `String` error so clap can surface it directly.
pub(crate) fn parse_relay_ladder(raw: &str) -> Result<Vec<RelayUrl>, String> {
    raw.split(',')
        .map(|entry| {
            let trimmed = entry.trim();
            if trimmed.is_empty() {
                return Err(format!("empty entry in relay ladder {raw:?}"));
            }
            trimmed
                .parse::<RelayUrl>()
                .map_err(|error| format!("invalid relay URL {trimmed:?}: {error}"))
        })
        .collect()
}

/// An ordered, non-empty relay ladder (`a,b,c` in preference order),
/// validated at construction. Public + **iroh-free**: the wrapped
/// `Vec<RelayUrl>` stays private, so embedders (`CreateConfig`) name a
/// ladder without depending on the `iroh` type. Parsing reuses
/// `parse_relay_ladder` — the same source of truth as the CLI value
/// parser — and rejects empty entries, so a `RelayLadder` is never empty;
/// "no custom ladder" is the `Option::None` case at the boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayLadder(Vec<RelayUrl>);

/// A relay ladder that couldn't be parsed (empty entry / invalid URL).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayLadderError(String);

impl fmt::Display for RelayLadderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for RelayLadderError {}

impl FromStr for RelayLadder {
    type Err = RelayLadderError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        parse_relay_ladder(input)
            .map(RelayLadder)
            .map_err(RelayLadderError)
    }
}

impl RelayLadder {
    /// The ordered rungs, for internal consumers — keeps `RelayUrl` off
    /// the public surface.
    pub(crate) fn as_urls(&self) -> &[RelayUrl] {
        &self.0
    }

    /// The number of rungs (always >= 1).
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Always `false` — a `RelayLadder` is constructed non-empty. Present
    /// for API completeness (and `clippy::len_without_is_empty`).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for RelayLadder {
    /// The canonical `a,b,c` text form — round-trips through [`FromStr`].
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, url) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str(",")?;
            }
            write!(formatter, "{url}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod lookup_tests {
    use super::{
        LookupOpts, LookupSet, MeshConfig, RelayChoice, RelayLadder, RelaySelection,
        resolve_lookups,
    };

    fn lookups(mdns: bool, dht: bool, relay: RelaySelection) -> LookupSet {
        LookupSet { mdns, dht, relay }
    }

    #[test]
    fn relay_ladder_parses_ordered_rungs() {
        let one: RelayLadder = "https://a.example".parse().unwrap();
        assert_eq!(one.len(), 1);
        assert!(!one.is_empty());

        let two: RelayLadder = "https://a.example,https://b.example".parse().unwrap();
        assert_eq!(two.len(), 2);
        // Display round-trips through FromStr (canonical `a,b` text form).
        let rendered = two.to_string();
        assert_eq!(rendered.parse::<RelayLadder>().unwrap(), two);
        assert_eq!(two.as_urls().len(), 2);
    }

    #[test]
    fn relay_ladder_rejects_empty_and_empty_entries() {
        assert!("".parse::<RelayLadder>().is_err());
        assert!(
            "https://a.example,,https://b.example"
                .parse::<RelayLadder>()
                .is_err(),
            "an empty entry must be rejected so a typo never shrinks the ladder"
        );
    }

    #[test]
    fn naming_relay_enables_it_without_public() {
        // Granular model: naming any lookup uses only those, regardless of
        // `public`. A relay alone yields a reachable (non-loopback) mesh.
        let ladder: RelayLadder = "https://a.example".parse().unwrap();
        let opts = resolve_lookups(false, lookups(false, false, RelaySelection::Custom(ladder)));
        assert!(!opts.mdns && !opts.dht);
        assert!(
            !opts.is_loopback(),
            "a named relay makes the mesh reachable"
        );
        assert!(matches!(opts.relay, RelayChoice::Custom(_)));
    }

    #[test]
    fn public_no_flags_enables_all_three() {
        let opts = resolve_lookups(true, LookupSet::default());
        assert!(opts.mdns && opts.dht);
        assert_eq!(opts.relay, RelayChoice::Pinned, "preset ⇒ pinned relay");
        assert!(!opts.is_loopback());
    }

    #[test]
    fn no_public_no_flags_is_loopback() {
        let opts = resolve_lookups(false, LookupSet::default());
        assert!(opts.is_loopback());
        assert_eq!(opts.network_label(), "private");
    }

    #[test]
    fn mdns_alone_disables_dht_and_relay() {
        let opts = resolve_lookups(false, lookups(true, false, RelaySelection::Unset));
        assert!(opts.mdns && !opts.dht);
        assert_eq!(
            opts.relay,
            RelayChoice::Disabled,
            "--mdns alone ⇒ relay off"
        );
        assert!(!opts.is_loopback(), "any lookup ⇒ reachable");
    }

    #[test]
    fn bare_relay_is_pinned_and_suppresses_lookups() {
        let opts = resolve_lookups(false, lookups(false, false, RelaySelection::Default));
        assert!(!opts.mdns && !opts.dht);
        assert_eq!(opts.relay, RelayChoice::Pinned);
    }

    #[test]
    fn valued_relay_preserves_ladder_order() {
        let rung0: iroh::RelayUrl = "https://a.example".parse().unwrap();
        let rung1: iroh::RelayUrl = "https://b.example".parse().unwrap();
        let ladder: RelayLadder = "https://a.example,https://b.example".parse().unwrap();
        let opts = resolve_lookups(false, lookups(false, false, RelaySelection::Custom(ladder)));
        assert_eq!(opts.relay, RelayChoice::Custom(vec![rung0, rung1]));
    }

    #[test]
    fn config_round_trips_loopback() {
        let config = MeshConfig::loopback();
        let decoded = MeshConfig::from_bytes(&config.to_bytes()).unwrap();
        assert_eq!(decoded, config);
    }

    #[test]
    fn config_round_trips_public_preset() {
        let config = MeshConfig::public_preset();
        let decoded = MeshConfig::from_bytes(&config.to_bytes()).unwrap();
        assert_eq!(decoded, config);
    }

    #[test]
    fn config_round_trips_custom_relay_ladder() {
        let config = MeshConfig {
            lookups: LookupOpts {
                mdns: true,
                dht: false,
                relay: RelayChoice::Custom(vec![
                    "https://a.example".parse().unwrap(),
                    "https://b.example".parse().unwrap(),
                ]),
            },
            password: None,
            issuer_pubkey: None,
        };
        let decoded = MeshConfig::from_bytes(&config.to_bytes()).unwrap();
        assert_eq!(decoded, config);
    }

    #[test]
    fn config_rejects_trailing_bytes() {
        // A lone trailing 0x00 is now the non-canonical zero feature byte;
        // either way it must be rejected, never silently absorbed.
        let mut bytes = MeshConfig::loopback().to_bytes();
        bytes.push(0);
        assert!(MeshConfig::from_bytes(&bytes).is_err());
    }

    #[test]
    fn config_rejects_custom_flag_without_enabled() {
        // flags with custom(0b1000) but not enabled(0b0100).
        let bytes = [0b1000];
        assert!(MeshConfig::from_bytes(&bytes).is_err());
    }

    #[test]
    fn config_round_trips_password_verifier() {
        let config = MeshConfig {
            lookups: LookupOpts::public_preset(),
            password: Some([0xA5u8; 16]),
            issuer_pubkey: None,
        };
        let decoded = MeshConfig::from_bytes(&config.to_bytes()).unwrap();
        assert_eq!(decoded, config);
    }

    #[test]
    fn passwordless_encoding_is_byte_identical_to_pre_feature_format() {
        // Live meshes depend on this: a config without a password must not
        // grow a feature byte (it feeds the topic derivation).
        assert_eq!(MeshConfig::loopback().to_bytes(), vec![0b0000]);
        assert_eq!(MeshConfig::public_preset().to_bytes(), vec![0b0111]);
    }

    #[test]
    fn config_rejects_unknown_feature_flags() {
        let mut bytes = MeshConfig::public_preset().to_bytes();
        bytes.push(0b0100); // an undefined feature bit (password=1, invite=2 are taken)
        bytes.extend_from_slice(&[0u8; 16]);
        let error = MeshConfig::from_bytes(&bytes).unwrap_err();
        assert!(
            error.to_string().contains("upgrade to a newer build"),
            "got: {error}"
        );
    }

    #[test]
    fn config_round_trips_issuer_pubkey() {
        let config = MeshConfig {
            lookups: LookupOpts::public_preset(),
            password: None,
            issuer_pubkey: Some([0x3Cu8; 32]),
        };
        let decoded = MeshConfig::from_bytes(&config.to_bytes()).unwrap();
        assert_eq!(decoded, config);
    }

    #[test]
    fn config_round_trips_password_and_invite_together() {
        // Both feature bits set: the fixed field order (verifier, then issuer
        // pubkey) must round-trip.
        let config = MeshConfig {
            lookups: LookupOpts::loopback(),
            password: Some([0xA5u8; 16]),
            issuer_pubkey: Some([0x3Cu8; 32]),
        };
        let decoded = MeshConfig::from_bytes(&config.to_bytes()).unwrap();
        assert_eq!(decoded, config);
    }

    #[test]
    fn config_rejects_truncated_issuer_pubkey() {
        let mut bytes = MeshConfig::public_preset().to_bytes();
        bytes.push(0b0010); // invite-only bit
        bytes.extend_from_slice(&[0u8; 16]); // half a pubkey
        assert!(MeshConfig::from_bytes(&bytes).is_err());
    }

    #[test]
    fn config_rejects_truncated_verifier() {
        let mut bytes = MeshConfig::public_preset().to_bytes();
        bytes.push(0b0001);
        bytes.extend_from_slice(&[0u8; 8]); // half a verifier
        assert!(MeshConfig::from_bytes(&bytes).is_err());
    }

    #[test]
    fn config_rejects_verifier_with_trailing_slack() {
        let mut bytes = MeshConfig::public_preset().to_bytes();
        bytes.push(0b0001);
        bytes.extend_from_slice(&[0u8; 17]); // verifier + one extra byte
        assert!(MeshConfig::from_bytes(&bytes).is_err());
    }
}

#[cfg(test)]
mod directory_selection_tests {
    use super::{DEFAULT_DIRECTORY, DirectorySelection, LookupOpts, MeshName, validate_advertise};

    #[test]
    fn unset_is_not_advertising() {
        let sel = DirectorySelection::Unset;
        assert!(!sel.is_set());
        assert!(sel.directory().is_none());
    }

    #[test]
    fn bare_resolves_to_default_directory() {
        let sel = DirectorySelection::Default;
        assert!(sel.is_set());
        assert_eq!(sel.directory().unwrap().as_str(), DEFAULT_DIRECTORY);
    }

    #[test]
    fn named_resolves_to_that_directory() {
        let sel = DirectorySelection::Named(MeshName::new("gamedev").unwrap());
        assert_eq!(sel.directory().unwrap().as_str(), "gamedev");
    }

    #[test]
    fn advertise_requires_reachable_mesh() {
        // Loopback-only + advertising is rejected.
        let error =
            validate_advertise(&DirectorySelection::Default, &LookupOpts::loopback()).unwrap_err();
        assert!(error.to_string().contains("reachable"), "got: {error}");
        // Reachable + advertising, and loopback + not advertising, are fine.
        assert!(
            validate_advertise(&DirectorySelection::Default, &LookupOpts::public_preset()).is_ok()
        );
        assert!(validate_advertise(&DirectorySelection::Unset, &LookupOpts::loopback()).is_ok());
    }
}
