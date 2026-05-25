//! [`SwarmMode`] — the network mode encoded in swarm identifiers — and
//! the "a relay is only meaningful on the public network" rule shared by
//! the relay-as-string create paths (MCP / embed).

use anyhow::{Result, bail};
use iroh::RelayUrl;

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
    pub(super) fn to_byte(self) -> u8 {
        match self {
            SwarmMode::Private => 0,
            SwarmMode::Public => 1,
        }
    }

    pub(super) fn from_byte(value: u8) -> Result<Self> {
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
/// the CLI uses [`super::validate_lookups`], which generalises this; for
/// callers that hand a relay as a string, prefer [`resolve_relay`]
/// (guard + parse).
pub(crate) fn require_relay_public(mode: SwarmMode, has_relay: bool) -> Result<()> {
    if has_relay && mode != SwarmMode::Public {
        bail!("a relay requires the public network");
    }
    Ok(())
}

/// Parse a comma-separated, ordered relay **ladder** (`a,b,c`) — order
/// preserved (the beacon homes on the first reachable rung); an empty or
/// whitespace-only entry is a hard error so a typo never silently
/// shrinks the ladder. The single source of truth for `--relay` syntax,
/// shared by the CLI value-parser and [`resolve_relay`] (the MCP/embed
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

/// Enforce [`require_relay_public`] then [`parse_relay_ladder`]. Used by
/// the create paths that take a relay as text (MCP tool args, embed
/// `CreateConfig`). `None` ⇒ an empty ladder (the caller's `default_for`
/// then falls back to the pinned default).
pub(crate) fn resolve_relay(mode: SwarmMode, relay: Option<&str>) -> Result<Vec<RelayUrl>> {
    require_relay_public(mode, relay.is_some())?;
    match relay {
        None => Ok(Vec::new()),
        Some(raw) => parse_relay_ladder(raw).map_err(|error| anyhow::anyhow!(error)),
    }
}
