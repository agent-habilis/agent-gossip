use clap::Parser;
use habilis_network::protocol::{LookupSet, OptFlag, RelayLadder, RelaySelection};
use habilis_network::runtime::tuning::directory_private_for_test;
use rmcp::schemars;
use serde::Deserialize;
use url::Url;

/// One networking lookup mechanism a gossip may use to find peers — the
/// `--lookup` allowlist. Naming any restricts to exactly those; naming none
/// falls back to the command's default (`create`: loopback, or the public preset with
/// `--advertise`; `discover`/`a2a expose`/`a2a discover`: the all-on public
/// preset — see "NETWORKING").
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
    /// Rendezvous and signalling through public Nostr relays.
    #[value(name = "nostr")]
    Nostr,
}

/// The `--lookup`/`--relay-url`/`--nostr-url` flags: naming any lookup uses *only* those
/// passed; naming none falls back to the command's default (`create`:
/// loopback; `discover`/`a2a expose`/`a2a discover`: the all-on public
/// preset).
#[derive(Parser, Debug)]
pub(crate) struct LookupArgs {
    /// Comma-separated networking lookups: mdns, dht, relay, nostr. An unknown
    /// value is an error.
    #[arg(long, value_enum, value_delimiter = ',')]
    pub lookup: Vec<Lookup>,

    /// Ordered, comma-separated relay ladder (the beacon homes on the first
    /// reachable rung), e.g. `https://a.example,https://b.example`. Requires
    /// `relay` in `--lookup`; omit for the default relay set.
    #[arg(long, value_name = "URL[,URL…]")]
    pub relay_url: Option<RelayLadder>,

    /// Comma-separated Nostr relays (`ws://` or `wss://`) that replace the
    /// pinned list, e.g. `wss://a.example,wss://b.example`. Requires `nostr`
    /// in `--lookup`; omit for the pinned relays. A URL cannot hold a comma.
    #[arg(long, value_name = "URL[,URL…]", value_delimiter = ',')]
    pub nostr_url: Vec<Url>,
}

impl LookupArgs {
    /// # Errors
    /// `--relay-url` was given without `relay` in `--lookup`.
    pub(crate) fn to_set(&self) -> anyhow::Result<LookupSet> {
        lookup_set(&self.lookup, self.relay_url.clone(), self.nostr_url.clone())
    }
}

/// The lookups a `create` uses. Listing a gossip in a directory
/// (`--advertise`) only makes sense for a mesh that other machines can reach,
/// so naming no lookup there takes the public preset, Nostr included.
///
/// The hidden `--directory-private` keeps the loopback lookups: the advertiser
/// reaches the directory over its own lookups, and those are baked into the
/// directory's gossip id, so a discoverer on the loopback ladder would never
/// meet an advertiser that took the public ones.
pub(crate) fn lookups_or_public(lookups: &[Lookup], advertise: bool) -> Vec<Lookup> {
    if advertise && lookups.is_empty() && !directory_private_for_test() {
        vec![Lookup::Mdns, Lookup::Dht, Lookup::Relay, Lookup::Nostr]
    } else {
        lookups.to_vec()
    }
}

/// Resolve `--lookup`/`--relay-url` into the [`LookupSet`] the engine's
/// `resolve_lookups` consumes. Shared by the CLI (via [`LookupArgs::to_set`])
/// and the MCP `create_gossip` tool so the two surfaces cannot drift.
///
/// # Errors
/// `relay_url` is given but `lookups` does not name `relay`, or `nostr_urls`
/// is not a valid custom Nostr list (the engine says why).
pub(crate) fn lookup_set(
    lookups: &[Lookup],
    relay_url: Option<RelayLadder>,
    nostr_urls: Vec<Url>,
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
    let mut set = LookupSet::default();
    set.mdns = lookups.contains(&Lookup::Mdns);
    set.dht = lookups.contains(&Lookup::Dht);
    set.relay_lookup = relay_lookup;
    if lookups.contains(&Lookup::Nostr) {
        set.nostr = OptFlag::Default;
    }
    if nostr_urls.is_empty() {
        Ok(set)
    } else {
        set.with_nostr_urls(nostr_urls).map_err(anyhow::Error::from)
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use habilis_network::protocol::{RelayChoice, RelayLadder, RelaySelection, resolve_lookups};

    use super::{Lookup, lookup_set};
    use crate::cli::args::{Cli, Commands};

    fn create_args(flags: &[&str]) -> Result<super::LookupArgs, clap::Error> {
        let mut command_line = vec!["agent-gossip", "create"];
        command_line.extend_from_slice(flags);
        Cli::try_parse_from(command_line).map(|cli| {
            let Commands::Create { options } = cli.command else {
                panic!("expected Create command");
            };
            options.lookups
        })
    }

    #[test]
    fn nostr_url_takes_a_comma_list_and_repeats() {
        let lookups = create_args(&[
            "--lookup",
            "nostr",
            "--nostr-url",
            "wss://a.example,wss://b.example",
            "--nostr-url",
            "wss://c.example",
        ])
        .expect("a comma list and a repeat both parse");
        assert_eq!(
            lookups.nostr_url.len(),
            3,
            "the comma list and the repeat make three URLs"
        );
        lookups
            .to_set()
            .expect("a custom Nostr list with the nostr lookup is valid");
    }

    #[test]
    fn nostr_url_with_a_comma_inside_is_a_parse_error() {
        let error = create_args(&["--lookup", "nostr", "--nostr-url", "wss://a.example/x,y"])
            .expect_err("the comma splits the URL, and `y` is not a URL");
        assert!(
            error.to_string().contains("--nostr-url"),
            "the error names the flag: {error}"
        );
    }

    #[test]
    fn nostr_url_without_the_nostr_lookup_is_the_engine_error() {
        let lookups = create_args(&["--lookup", "mdns", "--nostr-url", "wss://a.example"])
            .expect("the flags parse");
        let error = lookups.to_set().expect_err("a Nostr list needs `nostr`");
        assert!(
            error.to_string().contains("needs lookup `nostr`"),
            "the engine says why: {error}"
        );
    }

    #[test]
    fn lookup_list_resolves_to_only_named() {
        let set = lookup_set(&[Lookup::Mdns], None, Vec::new())
            .expect("this lookup combination is valid");
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
        let bare = lookup_set(&[Lookup::Relay], None, Vec::new())
            .expect("this lookup combination is valid");
        assert_eq!(
            bare.relay_lookup,
            RelaySelection::Default,
            "without a relay flag the relay lookup is the default"
        );

        let ladder: RelayLadder = "https://a.example,https://b.example"
            .parse()
            .expect("the test literal parses");
        let custom = lookup_set(&[Lookup::Relay], Some(ladder.clone()), Vec::new())
            .expect("this lookup combination is valid");
        assert_eq!(
            custom.relay_lookup,
            RelaySelection::Named(ladder),
            "a custom relay ladder is kept as given"
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
            lookup_set(&[Lookup::Mdns], Some(ladder), Vec::new()).is_err(),
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
