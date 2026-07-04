use clap::Parser;

use crate::protocol::swarm::{LookupSet, RelayLadder, RelaySelection};

/// The lookup allowlist flags: naming any uses *only* those passed (so
/// `--mdns` alone disables both dht and the relay); naming none falls
/// back to the command's default (`create`: loopback unless `--public`;
/// transfers/discover: the all-on public preset). Grouped and flattened
/// so each options struct stays within the readable bool budget.
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
    /// disables the relay; naming no flag at all falls back to the
    /// command's default. Absent ⇒ `None`; bare ⇒ `Some(None)`;
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

/// [`LookupArgs`] plus `--public`, for the commands whose no-flag default
/// is already the all-on public preset (transfers + directory browsing).
/// `create` keeps its own `--public` (there it opts *in* from a loopback
/// default and is baked into the swarm id).
#[derive(Parser, Debug)]
pub(crate) struct PublicLookupArgs {
    /// Explicitly select the all-on public preset (mDNS + DHT + the
    /// default relay ladder) — already the default when no lookup flag
    /// is named. Conflicts with the granular `--mdns`/`--dht`/`--relay`
    /// flags (they replace the preset) and, on the commands that take
    /// one, with a `--swarm`/ticket that already carries a discovery
    /// config.
    #[arg(long, default_value_t = false, conflicts_with_all = ["mdns", "dht", "relay"])]
    pub public: bool,

    #[command(flatten)]
    pub lookups: LookupArgs,
}

impl PublicLookupArgs {
    pub(crate) fn to_set(&self) -> LookupSet {
        if self.public {
            // The explicit alias for the no-flag default; the conflict
            // rule guarantees no granular flag accompanies it.
            LookupSet::default()
        } else {
            self.lookups.to_set()
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands};
    use crate::protocol::swarm::RelaySelection;

    /// Parse `agent-gossip create …` and read the resolved relay selection — the
    /// `--relay` allowlist flag lives in [`LookupArgs`], exercised here
    /// through the create command.
    fn relay_of(args: &[&str]) -> RelaySelection {
        match Cli::parse_from(args).command {
            Commands::Create { opts } => opts.lookups.to_set().relay,
            Commands::Join { .. }
            | Commands::Forum { .. }
            | Commands::Msg { .. }
            | Commands::Notice { .. }
            | Commands::Poll { .. }
            | Commands::Ping { .. }
            | Commands::Discover { .. }
            | Commands::Mcp { .. }
            | Commands::Man
            | Commands::Task { .. }
            | Commands::Peers { .. }
            | Commands::State { .. }
            | Commands::Meta { .. }
            | Commands::Ready { .. }
            | Commands::Plug { .. }
            | Commands::Unplug { .. }
            | Commands::Doctor { .. }
            | Commands::Leave { .. }
            | Commands::A2a { .. }
            | Commands::Session { .. } => panic!("expected Create"),
        }
    }

    #[test]
    fn relay_flag_absent_bare_and_valued() {
        assert_eq!(
            relay_of(&["agent-gossip", "create", "--public"]),
            RelaySelection::Unset,
            "absent ⇒ Unset"
        );
        assert_eq!(
            relay_of(&["agent-gossip", "create", "--public", "--relay"]),
            RelaySelection::Default,
            "bare ⇒ Default (pinned)"
        );
        assert_eq!(
            relay_of(&[
                "agent-gossip",
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
                "agent-gossip",
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
            "agent-gossip",
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
        // `agent-gossip create --mdns` ⇒ the swarm's id encodes mDNS only (naming a
        // lookup flag opts into exactly those; relay and dht stay off).
        let opts = match Cli::parse_from(["agent-gossip", "create", "--mdns"]).command {
            Commands::Create { opts } => opts,
            Commands::Join { .. }
            | Commands::Forum { .. }
            | Commands::Msg { .. }
            | Commands::Notice { .. }
            | Commands::Poll { .. }
            | Commands::Ping { .. }
            | Commands::Discover { .. }
            | Commands::Mcp { .. }
            | Commands::Man
            | Commands::Task { .. }
            | Commands::Peers { .. }
            | Commands::State { .. }
            | Commands::Meta { .. }
            | Commands::Ready { .. }
            | Commands::Plug { .. }
            | Commands::Unplug { .. }
            | Commands::Doctor { .. }
            | Commands::Leave { .. }
            | Commands::A2a { .. }
            | Commands::Session { .. } => panic!("expected Create"),
        };
        let lookups = resolve_lookups(opts.public, opts.lookups.to_set());
        assert!(lookups.mdns && !lookups.dht);
        assert_eq!(lookups.relay, RelayChoice::Disabled);
    }
}
