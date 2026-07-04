//! The swarm-wide config carried in the `🐝…` id — the lookup
//! allowlist (`mdns`/`dht`/`relay`) — plus its byte codec and the
//! `--advertise` directory selection. A swarm's network reach is fully
//! described by its lookups: no lookups means loopback-only; any lookup
//! means reachable across machines.

use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use iroh::RelayUrl;

use super::SwarmName;
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
pub(crate) enum RelayChoice {
    Disabled,
    Pinned,
    Custom(Vec<RelayUrl>),
}

/// The lookup allowlist baked into the swarm id. `mdns`/`dht` are the
/// enabled iroh address-lookups (both resolve the same seed-derived
/// `rendezvous_id`); `relay` is the connectivity relay (see
/// [`RelayChoice`]). An all-off set is a loopback-only swarm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LookupOpts {
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
    pub(crate) fn loopback() -> Self {
        LookupOpts {
            mdns: false,
            dht: false,
            relay: RelayChoice::Disabled,
        }
    }

    /// The all-on default for a swarm reachable across machines: both
    /// address-lookups plus the pinned default relay ladder.
    pub(crate) fn public_preset() -> Self {
        LookupOpts {
            mdns: true,
            dht: true,
            relay: RelayChoice::Pinned,
        }
    }

    /// True when nothing reaches off-machine — the swarm is loopback-only.
    pub(crate) fn is_loopback(&self) -> bool {
        !self.mdns && !self.dht && self.relay == RelayChoice::Disabled
    }

    /// Human/JSON label for the swarm's reach. Derived from the lookups —
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
    pub(crate) fn encode_into(&self, buf: &mut Vec<u8>) {
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
            // The ladder is created locally and bounded by the CLI/embed,
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
    pub(crate) fn decode_from(bytes: &[u8], pos: &mut usize) -> Result<Self> {
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
            RelayChoice::Custom(ladder)
        };
        Ok(LookupOpts { mdns, dht, relay })
    }
}

pub(super) fn read_u16(bytes: &[u8], pos: &mut usize) -> Result<u16> {
    let end = pos.checked_add(2).context("u16 length overflow")?;
    let slice = bytes.get(*pos..end).context("truncated u16")?;
    *pos = end;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

/// Feature byte appended after the lookups when a swarm has a password.
/// Appended — not a spare lookup-flags bit — because old binaries ignore
/// unknown flag bits (they would silently decode a passworded id and sit
/// in an empty topic) but hard-error on trailing config bytes.
const FEATURE_PASSWORD: u8 = 0b0001;

/// The swarm-wide configuration carried in the id and mixed into the
/// gossip topic, so every member that joins behaves identically. A
/// different config is a different swarm (different topic).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SwarmConfig {
    pub lookups: LookupOpts,
    /// The password verifier — a one-way check value derived from the
    /// Argon2id-stretched password (`crypto::password_verifier`), never the
    /// password itself. `None` ⇒ passwordless. Carried in the id so `join`
    /// can verify a candidate password locally before any network.
    pub password: Option<[u8; PASSWORD_VERIFIER_LEN]>,
}

impl SwarmConfig {
    /// Default loopback-only config: no lookups. Test-only since the
    /// directory now builds its config from explicit lookups and `create`
    /// constructs `SwarmConfig` directly.
    #[cfg(test)]
    pub(crate) fn loopback() -> Self {
        SwarmConfig {
            lookups: LookupOpts::loopback(),
            password: None,
        }
    }

    /// Default reachable-across-machines config: the all-on lookup preset.
    /// Test-only (see [`SwarmConfig::loopback`]).
    #[cfg(test)]
    pub(crate) fn public_preset() -> Self {
        SwarmConfig {
            lookups: LookupOpts::public_preset(),
            password: None,
        }
    }

    /// Canonical wire bytes: `[lookups…][if password: feature-flags u8 ‖
    /// verifier]`. This exact byte string is what the id carries and what
    /// the topic derivation mixes in, so it must be deterministic — the
    /// feature byte is emitted only when nonzero (a passwordless config
    /// stays byte-for-byte what it was before features existed).
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(2);
        self.lookups.encode_into(&mut buf);
        if let Some(verifier) = &self.password {
            buf.push(FEATURE_PASSWORD);
            buf.extend_from_slice(verifier);
        }
        buf
    }

    /// Decode a config region, requiring it to consume `bytes` exactly
    /// (no trailing slack within the length-delimited region we were given).
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut pos = 0;
        let lookups = LookupOpts::decode_from(bytes, &mut pos)?;
        let password = if pos == bytes.len() {
            None
        } else {
            let features = bytes[pos];
            pos += 1;
            if features & !FEATURE_PASSWORD != 0 {
                bail!("unsupported swarm feature flags {features:#04x} — upgrade ahsw");
            }
            if features == 0 {
                // A zero feature byte re-encodes without itself, silently
                // changing the topic-derivation bytes — reject the
                // non-canonical form outright.
                bail!("non-canonical swarm config: zero feature flags");
            }
            let end = pos
                .checked_add(PASSWORD_VERIFIER_LEN)
                .context("password verifier length overflow")?;
            let raw = bytes.get(pos..end).context("truncated password verifier")?;
            pos = end;
            let mut verifier = [0u8; PASSWORD_VERIFIER_LEN];
            verifier.copy_from_slice(raw);
            Some(verifier)
        };
        if pos != bytes.len() {
            bail!("trailing bytes in swarm config");
        }
        Ok(SwarmConfig { lookups, password })
    }
}

/// Relay intent in a [`LookupSet`]: absent / default / custom. Resolved
/// into a `RelayChoice` by `resolve_lookups`. `Custom` carries the
/// ordered [`RelayLadder`] (iroh-free), so this enum is part of the public
/// embed surface.
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

/// CLI `--advertise` intent: absent / bare / valued — the same
/// three-state optional-value shape as [`RelaySelection`]. `Unset` ⇒
/// the swarm is not listed in any directory; `Default` ⇒ the well-known
/// `global` directory; `Named` ⇒ a custom directory. The directory name is itself a
/// [`SwarmName`] (same charset), since the directory derives its
/// swarm from it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum DirectorySelection {
    #[default]
    Unset,
    Default,
    Named(SwarmName),
}

/// The well-known default directory — used when `--advertise` is passed
/// bare (no value).
pub(crate) const DEFAULT_DIRECTORY: &str = "global";

impl DirectorySelection {
    /// Resolve a clap three-state `--advertise` optional-value flag
    /// (absent / bare / valued) — the one converter shared by every
    /// command that advertises (`create`, `pipe listen`, `file send`,
    /// `port listen`).
    #[expect(
        clippy::option_option,
        reason = "clap optional-value flag: absent/bare/valued are three distinct directory states"
    )]
    pub(crate) fn from_flag(flag: Option<Option<SwarmName>>) -> Self {
        match flag {
            None => DirectorySelection::Unset,
            Some(None) => DirectorySelection::Default,
            Some(Some(directory)) => DirectorySelection::Named(directory),
        }
    }

    /// `true` when advertising is requested at all (bare or valued).
    pub(crate) fn is_set(&self) -> bool {
        !matches!(self, DirectorySelection::Unset)
    }

    /// The directory to advertise into, or `None` when not advertising.
    /// Bare ⇒ the [`DEFAULT_DIRECTORY`]; valued ⇒ the given name.
    pub(crate) fn directory(&self) -> Option<SwarmName> {
        match self {
            DirectorySelection::Unset => None,
            DirectorySelection::Default => Some(
                SwarmName::new(DEFAULT_DIRECTORY).expect("DEFAULT_DIRECTORY is a valid swarm name"),
            ),
            DirectorySelection::Named(name) => Some(name.clone()),
        }
    }
}

/// Advertise was requested on a loopback-only swarm. A directory listing
/// requires a swarm reachable across machines, so this is a hard error
/// (never a silent no-op). Typed so callers can classify it — the MCP
/// server maps it to `invalid_params`, the CLI to an `anyhow` bail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdvertiseRequiresReachable;

impl fmt::Display for AdvertiseRequiresReachable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("advertise needs a reachable swarm; enable a lookup (e.g. public)")
    }
}

impl std::error::Error for AdvertiseRequiresReachable {}

/// `--advertise` lists the swarm in a public directory, so it requires a
/// swarm that is actually reachable across machines.
pub(crate) fn validate_advertise(
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
/// the swarm id. Naming **any** lookup flag uses *only* those passed (so
/// `--mdns` alone is mDNS-only, relay/dht off); naming **none** but
/// passing `--public` enables the all-on preset; naming nothing at all is
/// a loopback-only swarm. `--relay` bare ⇒ pinned default, `--relay
/// <url>` ⇒ custom ladder.
pub(crate) fn resolve_lookups(public: bool, lookups: LookupSet) -> LookupOpts {
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

/// Resolve a transfer command's (`pipe`/`file`/`port`/`sh`/`mount`)
/// discovery config from its two alternative sources: a `--swarm 🐝…` id
/// (whose embedded lookups win) or the create-style
/// `--mdns/--dht/--relay` flags (naming any uses only those). Unlike
/// [`resolve_lookups`], naming **nothing** is the all-on public preset —
/// a transfer is inherently networked, so its default is public where
/// `create`'s is loopback. `--public`, the preset's explicit alias, and
/// the `--swarm`-vs-flags exclusivity are enforced by clap; the both-
/// sources bail below is a backstop for non-CLI callers.
///
/// # Errors
/// Both sources given (ambiguous), or an invalid `--swarm` id.
pub(crate) fn resolve_transfer_lookups(
    swarm: Option<&str>,
    flags: LookupSet,
) -> Result<LookupOpts> {
    match swarm {
        Some(id) => {
            if flags.any() {
                bail!(
                    "--swarm already carries a discovery config; \
                     drop the --mdns/--dht/--relay flags"
                );
            }
            Ok(id
                .parse::<super::Swarm>()
                .context("invalid --swarm id")?
                .lookups()
                .clone())
        }
        None => Ok(resolve_lookups(true, flags)),
    }
}

/// Parse a comma-separated, ordered relay **ladder** (`a,b,c`) — order
/// preserved (the beacon homes on the first reachable rung); an empty or
/// whitespace-only entry is a hard error so a typo never silently
/// shrinks the ladder. The single source of truth for `--relay` syntax,
/// shared by the CLI value-parser and `RelayLadder` (the MCP/embed
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
        LookupOpts, LookupSet, RelayChoice, RelayLadder, RelaySelection, SwarmConfig,
        resolve_lookups, resolve_transfer_lookups,
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
        // `public`. A relay alone yields a reachable (non-loopback) swarm.
        let ladder: RelayLadder = "https://a.example".parse().unwrap();
        let opts = resolve_lookups(false, lookups(false, false, RelaySelection::Custom(ladder)));
        assert!(!opts.mdns && !opts.dht);
        assert!(
            !opts.is_loopback(),
            "a named relay makes the swarm reachable"
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
    fn transfer_no_flags_is_the_public_preset() {
        let opts = resolve_transfer_lookups(None, LookupSet::default()).unwrap();
        assert_eq!(opts, LookupOpts::public_preset());
    }

    #[test]
    fn transfer_named_flags_restrict_to_those() {
        let opts =
            resolve_transfer_lookups(None, lookups(true, false, RelaySelection::Unset)).unwrap();
        assert!(opts.mdns && !opts.dht);
        assert_eq!(opts.relay, RelayChoice::Disabled);
    }

    #[test]
    fn transfer_swarm_id_wins_and_rejects_flags() {
        let id = super::super::Swarm::new(
            [7u8; super::super::SEED_LEN],
            super::super::SwarmName::new("test").unwrap(),
            SwarmConfig::loopback(),
        )
        .to_string();
        // The id's embedded lookups win when no flag is passed.
        let opts = resolve_transfer_lookups(Some(&id), LookupSet::default()).unwrap();
        assert!(opts.is_loopback());
        // Both sources at once is ambiguous (clap rejects it first on the CLI).
        let error =
            resolve_transfer_lookups(Some(&id), lookups(true, false, RelaySelection::Unset))
                .unwrap_err();
        assert!(error.to_string().contains("--swarm"), "got: {error}");
    }

    #[test]
    fn config_round_trips_loopback() {
        let config = SwarmConfig::loopback();
        let decoded = SwarmConfig::from_bytes(&config.to_bytes()).unwrap();
        assert_eq!(decoded, config);
    }

    #[test]
    fn config_round_trips_public_preset() {
        let config = SwarmConfig::public_preset();
        let decoded = SwarmConfig::from_bytes(&config.to_bytes()).unwrap();
        assert_eq!(decoded, config);
    }

    #[test]
    fn config_round_trips_custom_relay_ladder() {
        let config = SwarmConfig {
            lookups: LookupOpts {
                mdns: true,
                dht: false,
                relay: RelayChoice::Custom(vec![
                    "https://a.example".parse().unwrap(),
                    "https://b.example".parse().unwrap(),
                ]),
            },
            password: None,
        };
        let decoded = SwarmConfig::from_bytes(&config.to_bytes()).unwrap();
        assert_eq!(decoded, config);
    }

    #[test]
    fn config_rejects_trailing_bytes() {
        // A lone trailing 0x00 is now the non-canonical zero feature byte;
        // either way it must be rejected, never silently absorbed.
        let mut bytes = SwarmConfig::loopback().to_bytes();
        bytes.push(0);
        assert!(SwarmConfig::from_bytes(&bytes).is_err());
    }

    #[test]
    fn config_rejects_custom_flag_without_enabled() {
        // flags with custom(0b1000) but not enabled(0b0100).
        let bytes = [0b1000];
        assert!(SwarmConfig::from_bytes(&bytes).is_err());
    }

    #[test]
    fn config_round_trips_password_verifier() {
        let config = SwarmConfig {
            lookups: LookupOpts::public_preset(),
            password: Some([0xA5u8; 16]),
        };
        let decoded = SwarmConfig::from_bytes(&config.to_bytes()).unwrap();
        assert_eq!(decoded, config);
    }

    #[test]
    fn passwordless_encoding_is_byte_identical_to_pre_feature_format() {
        // Live swarms depend on this: a config without a password must not
        // grow a feature byte (it feeds the topic derivation).
        assert_eq!(SwarmConfig::loopback().to_bytes(), vec![0b0000]);
        assert_eq!(SwarmConfig::public_preset().to_bytes(), vec![0b0111]);
    }

    #[test]
    fn config_rejects_unknown_feature_flags() {
        let mut bytes = SwarmConfig::public_preset().to_bytes();
        bytes.push(0b0010); // an undefined feature bit
        bytes.extend_from_slice(&[0u8; 16]);
        let error = SwarmConfig::from_bytes(&bytes).unwrap_err();
        assert!(error.to_string().contains("upgrade ahsw"), "got: {error}");
    }

    #[test]
    fn config_rejects_truncated_verifier() {
        let mut bytes = SwarmConfig::public_preset().to_bytes();
        bytes.push(0b0001);
        bytes.extend_from_slice(&[0u8; 8]); // half a verifier
        assert!(SwarmConfig::from_bytes(&bytes).is_err());
    }

    #[test]
    fn config_rejects_verifier_with_trailing_slack() {
        let mut bytes = SwarmConfig::public_preset().to_bytes();
        bytes.push(0b0001);
        bytes.extend_from_slice(&[0u8; 17]); // verifier + one extra byte
        assert!(SwarmConfig::from_bytes(&bytes).is_err());
    }
}

#[cfg(test)]
mod directory_selection_tests {
    use super::{DEFAULT_DIRECTORY, DirectorySelection, LookupOpts, SwarmName, validate_advertise};

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
        let sel = DirectorySelection::Named(SwarmName::new("gamedev").unwrap());
        assert_eq!(sel.directory().unwrap().as_str(), "gamedev");
    }

    #[test]
    fn advertise_requires_reachable_swarm() {
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
