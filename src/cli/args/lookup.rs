//! The `--mdns`/`--dht`/`--relay` lookup allowlist flag group, flattened
//! into every server command, and its resolution into a [`LookupSet`].

use clap::Parser;

use crate::protocol::swarm::{LookupSet, RelayLadder, RelaySelection};

/// The lookup allowlist flags: with `--public`, naming none enables
/// all three (mdns + dht + pinned relay); naming any uses *only* those
/// passed (so `--mdns` alone disables both dht and the relay). All
/// require `--public`. Grouped and flattened so each options struct
/// stays within the readable bool budget.
#[derive(Parser, Debug)]
pub(crate) struct LookupArgs {
    /// Enable the LAN mDNS address-lookup.
    #[arg(long, default_value_t = false)]
    pub mdns: bool,

    /// Enable the mainline-DHT address-lookup.
    #[arg(long, default_value_t = false)]
    pub dht: bool,

    /// Enable the relay (connectivity + relay-direct rendezvous). Bare
    /// `--relay` ⇒ the default n0 prod relay *ladder*; `--relay
    /// <URL>[,<URL>…]` ⇒ a custom ordered ladder (the beacon homes on
    /// the first reachable rung). Omitting it while naming another flag
    /// disables the relay; naming no flag at all enables the default
    /// ladder. An allowlist member like `--mdns`/`--dht` — per-process,
    /// requires `--public`. Absent ⇒ `None`; bare ⇒ `Some(None)`;
    /// valued ⇒ `Some(Some(ladder))`.
    #[arg(long, num_args(0..=1))]
    #[expect(
        clippy::option_option,
        reason = "clap optional-value flag: absent/bare/valued are three distinct relay states (see RelaySelection)"
    )]
    pub relay: Option<Option<RelayLadder>>,
}

impl LookupArgs {
    pub(crate) fn to_set(&self) -> LookupSet {
        let relay = match &self.relay {
            None => RelaySelection::Unset,
            Some(None) => RelaySelection::Default,
            Some(Some(ladder)) => RelaySelection::Custom(ladder.clone()),
        };
        LookupSet {
            mdns: self.mdns,
            dht: self.dht,
            relay,
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands};
    use crate::protocol::swarm::RelaySelection;

    /// Parse `ahs create …` and read the resolved relay selection — the
    /// `--relay` allowlist flag lives in [`LookupArgs`], exercised here
    /// through the create command.
    fn relay_of(args: &[&str]) -> RelaySelection {
        match Cli::parse_from(args).command {
            Commands::Create { opts } => opts.lookups.to_set().relay,
            Commands::Join { .. }
            | Commands::Msg { .. }
            | Commands::Poll { .. }
            | Commands::Ping { .. }
            | Commands::Discover { .. }
            | Commands::Mcp
            | Commands::Man
            | Commands::Exchange { .. }
            | Commands::Peers { .. }
            | Commands::Setup { .. }
            | Commands::Teardown { .. }
            | Commands::Status => panic!("expected Create"),
        }
    }

    #[test]
    fn relay_flag_absent_bare_and_valued() {
        assert_eq!(
            relay_of(&["ahs", "create", "--public"]),
            RelaySelection::Unset,
            "absent ⇒ Unset"
        );
        assert_eq!(
            relay_of(&["ahs", "create", "--public", "--relay"]),
            RelaySelection::Default,
            "bare ⇒ Default (pinned)"
        );
        assert_eq!(
            relay_of(&[
                "ahs",
                "create",
                "--public",
                "--relay",
                "https://relay.example"
            ]),
            RelaySelection::Custom("https://relay.example".parse().unwrap()),
            "valued ⇒ single-rung Custom ladder"
        );
        assert_eq!(
            relay_of(&[
                "ahs",
                "create",
                "--public",
                "--relay",
                "https://a.example,https://b.example"
            ]),
            RelaySelection::Custom("https://a.example,https://b.example".parse().unwrap()),
            "comma-separated ⇒ ordered multi-rung ladder"
        );
    }

    #[test]
    fn relay_flag_rejects_empty_ladder_entry() {
        let parsed = Cli::try_parse_from([
            "ahs",
            "create",
            "--public",
            "--relay",
            "https://a.example,,https://b.example",
        ]);
        assert!(parsed.is_err(), "empty entry must be rejected");
    }

    #[test]
    fn create_mdns_resolves_to_mdns_only_lookups() {
        use crate::protocol::swarm::{RelayChoice, resolve_lookups};
        // `ahs create --mdns` ⇒ the swarm's id encodes mDNS only (naming a
        // lookup flag opts into exactly those; relay and dht stay off).
        let opts = match Cli::parse_from(["ahs", "create", "--mdns"]).command {
            Commands::Create { opts } => opts,
            Commands::Join { .. }
            | Commands::Msg { .. }
            | Commands::Poll { .. }
            | Commands::Ping { .. }
            | Commands::Discover { .. }
            | Commands::Mcp
            | Commands::Man
            | Commands::Exchange { .. }
            | Commands::Peers { .. }
            | Commands::Setup { .. }
            | Commands::Teardown { .. }
            | Commands::Status => panic!("expected Create"),
        };
        let lookups = resolve_lookups(opts.public, opts.lookups.to_set());
        assert!(lookups.mdns && !lookups.dht);
        assert_eq!(lookups.relay, RelayChoice::Disabled);
    }
}
