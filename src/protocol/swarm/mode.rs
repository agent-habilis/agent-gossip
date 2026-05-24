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
