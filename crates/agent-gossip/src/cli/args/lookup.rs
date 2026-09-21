use clap::Parser;
use rmcp::schemars;
use serde::Deserialize;

use fofoca::protocol::{
    LookupOpts, LookupSet, RelayChoice, RelayLadder, RelaySelection, TransportPolicy,
};

/// One networking lookup mechanism a gossip may use to find peers — the
/// `--lookup` allowlist. Naming any restricts to exactly those; naming none
/// falls back to the command's default (`create`: loopback; `discover`/`a2a
/// expose`/`a2a discover`: the all-on public preset — see "NETWORKING").
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Lookup {
    /// LAN mDNS multicast.
    #[value(name = "mdns")]
    Mdns,
    /// Mainline `BitTorrent` DHT.
    #[value(name = "dht")]
    Dht,
    /// Relay connectivity + the relay-direct rendezvous dial.
    #[value(name = "relay")]
    Relay,
}

/// A payload path a mesh's members may use — the `--transport` policy.
/// `create`-only; every joiner inherits it from the mesh id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Transport {
    /// Direct paths only (hole-punched IP or `WebRTC`). The default.
    #[value(name = "p2p")]
    P2p,
    /// Let payload also fall back to the relay when a direct path fails.
    #[value(name = "relay")]
    Relay,
}

/// The `--lookup`/`--relay-url` flags: naming any lookup uses *only* those
/// passed; naming none falls back to the command's default (`create`:
/// loopback; `discover`/`a2a expose`/`a2a discover`: the all-on public
/// preset).
#[derive(Parser, Debug)]
pub(crate) struct LookupArgs {
    /// Comma-separated networking lookups: mdns, dht, relay. An unknown
    /// value is an error.
    #[arg(long, value_enum, value_delimiter = ',')]
    pub lookup: Vec<Lookup>,

    /// Ordered, comma-separated relay ladder (the beacon homes on the first
    /// reachable rung), e.g. `https://a.example,https://b.example`. Requires
    /// `relay` in `--lookup`; omit for the default relay set.
    #[arg(long, value_name = "URL[,URL…]")]
    pub relay_url: Option<RelayLadder>,
}

impl LookupArgs {
    /// # Errors
    /// `--relay-url` was given without `relay` in `--lookup`.
    pub(crate) fn to_set(&self) -> anyhow::Result<LookupSet> {
        lookup_set(&self.lookup, self.relay_url.clone())
    }
}

/// Resolve `--lookup`/`--relay-url` into the [`LookupSet`] the engine's
/// `resolve_lookups` consumes. Shared by the CLI (via [`LookupArgs::to_set`])
/// and the MCP `create_gossip` tool so the two surfaces cannot drift.
///
/// # Errors
/// `relay_url` is given but `lookups` does not name `relay`.
pub(crate) fn lookup_set(
    lookups: &[Lookup],
    relay_url: Option<RelayLadder>,
) -> anyhow::Result<LookupSet> {
    let relay = lookups.contains(&Lookup::Relay);
    if relay_url.is_some() && !relay {
        anyhow::bail!("--relay-url needs `relay` in --lookup");
    }
    let relay_lookup = if relay {
        relay_url.map_or(RelaySelection::Default, RelaySelection::Named)
    } else {
        RelaySelection::Unset
    };
    Ok(LookupSet {
        mdns: lookups.contains(&Lookup::Mdns),
        dht: lookups.contains(&Lookup::Dht),
        relay_lookup,
    })
}

/// Resolve `--transport` into a [`TransportPolicy`]. `create`-only. Shared by
/// the CLI and the MCP `create_gossip` tool.
///
/// # Errors
/// The list is non-empty and does not name `p2p`.
pub(crate) fn transport_policy(transports: &[Transport]) -> anyhow::Result<TransportPolicy> {
    if transports.is_empty() {
        return Ok(TransportPolicy::default());
    }
    if !transports.contains(&Transport::P2p) {
        anyhow::bail!("p2p cannot be disabled: use `p2p` or `p2p,relay`");
    }
    Ok(TransportPolicy {
        relay_transport: transports.contains(&Transport::Relay),
    })
}

/// The one cross-field rule between `--transport` and `--lookup`: letting the
/// relay carry payload needs a relay to exist as a lookup. The engine checks
/// the same rule in `MeshConfig::validate`, but names its own field there —
/// this check runs first, naming the flags.
///
/// # Errors
/// `policy.relay_transport` is set but `lookups.relay_lookup` is `Disabled`.
pub(crate) fn check_relay_transport(
    policy: TransportPolicy,
    lookups: &LookupOpts,
) -> anyhow::Result<()> {
    if policy.relay_transport && lookups.relay_lookup == RelayChoice::Disabled {
        anyhow::bail!("--transport p2p,relay needs a relay lookup: add `relay` to --lookup");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Lookup, Transport, check_relay_transport, lookup_set, transport_policy};
    use crate::cli::args::Cli;
    use fofoca::protocol::{
        LookupOpts, RelayChoice, RelayLadder, RelaySelection, TransportPolicy, resolve_lookups,
    };

    #[test]
    fn lookup_list_resolves_to_only_named() {
        let set = lookup_set(&[Lookup::Mdns], None).unwrap();
        assert!(set.mdns && !set.dht);
        assert_eq!(set.relay_lookup, RelaySelection::Unset);
        let opts = resolve_lookups(false, set);
        assert!(opts.mdns && !opts.dht);
        assert_eq!(opts.relay_lookup, RelayChoice::Disabled);
    }

    #[test]
    fn lookup_relay_default_and_custom_ladder() {
        let bare = lookup_set(&[Lookup::Relay], None).unwrap();
        assert_eq!(bare.relay_lookup, RelaySelection::Default);

        let ladder: RelayLadder = "https://a.example,https://b.example".parse().unwrap();
        let custom = lookup_set(&[Lookup::Relay], Some(ladder.clone())).unwrap();
        assert_eq!(custom.relay_lookup, RelaySelection::Named(ladder));
    }

    #[test]
    fn lookup_rejects_unknown_value() {
        assert!(Cli::try_parse_from(["agent-gossip", "create", "--lookup", "relai"]).is_err());
    }

    #[test]
    fn relay_url_rejects_empty_entry() {
        let parsed = Cli::try_parse_from([
            "agent-gossip",
            "create",
            "--lookup",
            "relay",
            "--relay-url",
            "https://a.example,,https://b.example",
        ]);
        assert!(parsed.is_err(), "empty entry must be rejected");
    }

    #[test]
    fn relay_url_without_relay_lookup_errors() {
        let ladder: RelayLadder = "https://a.example".parse().unwrap();
        assert!(lookup_set(&[Lookup::Mdns], Some(ladder)).is_err());
    }

    #[test]
    fn removed_flags_are_rejected() {
        for flag in ["--public", "--mdns", "--dht", "--relay"] {
            assert!(
                Cli::try_parse_from(["agent-gossip", "create", flag]).is_err(),
                "{flag} must be rejected"
            );
        }
    }

    #[test]
    fn transport_absent_and_p2p_are_lookup_only() {
        assert_eq!(transport_policy(&[]).unwrap(), TransportPolicy::default());
        assert_eq!(
            transport_policy(&[Transport::P2p]).unwrap(),
            TransportPolicy {
                relay_transport: false
            }
        );
    }

    #[test]
    fn transport_p2p_relay_sets_relay_transport() {
        let policy = transport_policy(&[Transport::P2p, Transport::Relay]).unwrap();
        assert!(policy.relay_transport);
    }

    #[test]
    fn transport_relay_alone_errors() {
        assert!(transport_policy(&[Transport::Relay]).is_err());
    }

    #[test]
    fn transport_rejects_unknown_value() {
        assert!(Cli::try_parse_from(["agent-gossip", "create", "--transport", "webrtc"]).is_err());
    }

    #[test]
    fn relay_transport_without_relay_lookup_errors() {
        let policy = TransportPolicy {
            relay_transport: true,
        };
        let opts = LookupOpts::loopback();
        assert!(check_relay_transport(policy, &opts).is_err());
    }
}
