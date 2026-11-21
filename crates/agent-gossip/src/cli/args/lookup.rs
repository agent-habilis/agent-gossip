use clap::Parser;
use habilis_network::protocol::{LookupSet, RelayLadder, RelaySelection};
use habilis_network::runtime::tuning::directory_private_for_test;
use rmcp::schemars;
use serde::Deserialize;

/// One networking lookup mechanism a gossip may use to find peers — the
/// `--lookup` allowlist. Naming any restricts to exactly those; naming none
/// falls back to the command's default (`create`, `discover`, `a2a expose` and
/// `a2a discover`: the all-on public preset — see "NETWORKING").
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Lookup {
    /// LAN mDNS multicast.
    #[value(name = "mdns")]
    Mdns,
    /// Mainline `BitTorrent` DHT.
    #[value(name = "dht")]
    Dht,
    /// Rendezvous through a relay server. A lookup only: it does not make the
    /// relay carry payload.
    #[value(name = "relay")]
    Relay,
    /// No lookup: the gossip stays on this machine. `create` only; it stands
    /// alone in the list.
    #[value(name = "loopback")]
    Loopback,
}

/// The `--lookup`/`--relay-url` flags: naming any lookup uses *only* those
/// passed; naming none falls back to the command's default, the all-on public
/// preset.
#[derive(Parser, Debug)]
pub(crate) struct LookupArgs {
    /// Comma-separated networking lookups: mdns, dht, relay, or loopback
    /// (`create` only, alone). An unknown value is an error. Only the beacon
    /// publishes to the DHT. With `relay` in the lookups, the record holds the
    /// relay URL and no IP address. With `dht` and no `relay`, the beacon puts
    /// its IP addresses in the public DHT.
    #[arg(long, value_enum, value_delimiter = ',')]
    pub lookup: Vec<Lookup>,

    /// Ordered, comma-separated relay ladder (the beacon homes on the first
    /// reachable rung), e.g. `https://a.example,https://b.example`. Requires
    /// `relay` in `--lookup`; omit for the default relay set.
    #[arg(long, value_name = "URL[,URL…]")]
    pub relay_url: Option<RelayLadder>,
}

impl LookupArgs {
    /// The lookups of a command that reaches a directory or a bridge. An empty
    /// set resolves to the public preset there, so `loopback` would become
    /// public: it is refused, and `create` is the one command that honors it.
    ///
    /// # Errors
    /// `--relay-url` was given without `relay` in `--lookup`, or `--lookup`
    /// names `loopback`.
    pub(crate) fn to_set(&self) -> anyhow::Result<LookupSet> {
        anyhow::ensure!(
            !self.lookup.contains(&Lookup::Loopback),
            "--lookup loopback is for `create` only: this command takes mdns, dht or relay, or all three when it names none"
        );
        lookup_set(&self.lookup, self.relay_url.clone())
    }
}

/// The lookups a `create` uses. Naming no lookup takes all three: a gossip
/// that other machines can reach is the default, and `--lookup loopback`
/// keeps it on this machine.
///
/// The hidden `--directory-private` keeps the loopback lookups: the advertiser
/// reaches the directory over its own lookups, and those are baked into the
/// directory's gossip id, so a discoverer on the loopback ladder would never
/// meet an advertiser that took the public ones.
pub(crate) fn lookups_or_public(lookups: &[Lookup]) -> Vec<Lookup> {
    if lookups.is_empty() && !directory_private_for_test() {
        vec![Lookup::Mdns, Lookup::Dht, Lookup::Relay]
    } else {
        lookups.to_vec()
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
    if lookups.contains(&Lookup::Loopback) && lookups.len() > 1 {
        anyhow::bail!("--lookup loopback stands alone: it cannot combine with another lookup");
    }
    let relay_lookup = if relay {
        relay_url.map_or(RelaySelection::Default, RelaySelection::Named)
    } else {
        RelaySelection::Unset
    };
    let mut set = LookupSet::default();
    set.mdns = lookups.contains(&Lookup::Mdns);
    set.dht = lookups.contains(&Lookup::Dht);
    set.relay_lookup = relay_lookup;
    Ok(set)
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use habilis_network::protocol::{RelayChoice, RelayLadder, RelaySelection, resolve_lookups};

    use super::{Lookup, lookup_set};
    use crate::cli::args::Cli;

    #[test]
    fn lookup_list_resolves_to_only_named() {
        let set = lookup_set(&[Lookup::Mdns], None).expect("this lookup combination is valid");
        assert!(
            set.mdns && !set.dht,
            "a bare --lookup mdns sets mdns and not dht"
        );
        assert_eq!(
            set.relay_lookup,
            RelaySelection::Unset,
            "no relay lookup is set without a relay flag"
        );
        let options = resolve_lookups(set);
        assert!(
            options.mdns && !options.dht,
            "the options carry mdns on and dht off"
        );
        assert_eq!(
            options.relay_lookup,
            RelayChoice::Disabled,
            "a disabled relay lookup is kept as disabled"
        );
    }

    #[test]
    fn lookup_relay_default_and_custom_ladder() {
        let bare = lookup_set(&[Lookup::Relay], None).expect("this lookup combination is valid");
        assert_eq!(
            bare.relay_lookup,
            RelaySelection::Default,
            "without a relay flag the relay lookup is the default"
        );

        let ladder: RelayLadder = "https://a.example,https://b.example"
            .parse()
            .expect("the test literal parses");
        let custom = lookup_set(&[Lookup::Relay], Some(ladder.clone()))
            .expect("this lookup combination is valid");
        assert_eq!(
            custom.relay_lookup,
            RelaySelection::Named(ladder),
            "a custom relay ladder is kept as given"
        );
    }

    #[test]
    fn directory_commands_reject_loopback() {
        let args = super::LookupArgs {
            lookup: vec![Lookup::Loopback],
            relay_url: None,
        };
        assert!(
            args.to_set().is_err(),
            "an empty set would resolve to the public preset, so loopback cannot be honored here"
        );
    }

    #[test]
    fn lookup_rejects_unknown_value() {
        assert!(
            Cli::try_parse_from(["agent-gossip", "create", "--lookup", "relai"]).is_err(),
            "an unknown lookup name is rejected"
        );
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
        let ladder: RelayLadder = "https://a.example"
            .parse()
            .expect("the test literal parses");
        assert!(
            lookup_set(&[Lookup::Mdns], Some(ladder)).is_err(),
            "mdns cannot combine with a custom relay ladder"
        );
    }

    #[test]
    fn removed_flags_are_rejected() {
        for flag in ["--public", "--mdns", "--dht", "--relay"] {
            assert!(
                Cli::try_parse_from(["agent-gossip", "create", flag]).is_err(),
                "the removed {flag} must fail loudly so old scripts do not run with a silent default"
            );
        }
    }
}
