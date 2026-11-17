use habilis_network::protocol::meshes::NostrChoice;
use habilis_network::protocol::{
    LookupOpts, LookupSet, MeshConfig, RelayChoice, RelayLadder, TransportPolicy, resolve_lookups,
};
use url::Url;

use super::lookup::{Lookup, lookup_set};
use super::transport::{Transport, transport_policy};
use crate::policy;

pub(crate) struct Resolved {
    pub set: LookupSet,
    pub lookups: LookupOpts,
    pub transport: TransportPolicy,
}

/// Resolve the `create` flags into the parts of a mesh config. Shared by the
/// CLI and the MCP `create_gossip` tool so the two surfaces cannot drift.
///
/// An absent `--transport` takes the app default for these lookups. A given
/// list is literal: the relay carries payload only if the list names it.
///
/// The one rule that needs both concepts lives here: letting the relay carry
/// payload needs a relay to exist as a lookup. The engine checks the same rule
/// in `MeshConfig::validate`, but names its own fields there — this check runs
/// first, naming the flags.
///
/// A custom `--nostr-url` list with no `--transport` also turns the Nostr
/// transport on, as the engine does: the list is the only reason to name it.
///
/// # Errors
/// `relay_url` is given without `relay` in `lookups`, `nostr_urls` is given
/// without `nostr` in `lookups` or is not a valid list, `transports` is
/// invalid (see [`transport_policy`]), `transports` names `relay` while no
/// relay lookup is on, or the engine refuses the whole config (`nostr` in
/// `transports` without `nostr` in `lookups` and a `--nostr-url` list).
pub(crate) fn resolve(
    lookups: &[Lookup],
    relay_url: Option<RelayLadder>,
    nostr_urls: Vec<Url>,
    transports: &[Transport],
) -> anyhow::Result<Resolved> {
    let set = lookup_set(lookups, relay_url, nostr_urls)?;
    let resolved = resolve_lookups(set.clone());
    let transport = if transports.is_empty() {
        let policy = policy::app_transport_policy(&set);
        if matches!(resolved.nostr, NostrChoice::Custom(_)) {
            policy.nostr_transport(true)
        } else {
            policy
        }
    } else {
        transport_policy(transports)?
    };
    if transport.allows(habilis_network::protocol::Transport::Relay)
        && resolved.relay_lookup == RelayChoice::Disabled
    {
        anyhow::bail!("--transport relay needs a relay lookup: add `relay` to --lookup");
    }
    MeshConfig::new(resolved.clone(), None, None, transport.clone()).validate()?;
    Ok(Resolved {
        set,
        lookups: resolved,
        transport,
    })
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use habilis_network::protocol::{LookupOpts, Mesh, MeshConfig, RelayChoice, TransportPolicy};

    use super::{Resolved, resolve};
    use crate::cli::args::lookup::{Lookup, lookups_or_public};
    use crate::cli::args::transport::Transport;
    use crate::cli::args::{Cli, Commands};

    #[test]
    fn relay_transport_without_relay_lookup_errors() {
        let relay = [Transport::Udp, Transport::WebRtc, Transport::Relay];
        assert!(
            resolve(&[Lookup::Mdns], None, Vec::new(), &relay).is_err(),
            "an mdns lookup without a relay is rejected"
        );
    }

    fn policy(multihop: bool, relay_transport: bool) -> TransportPolicy {
        TransportPolicy::default()
            .multihop(multihop)
            .relay_transport(relay_transport)
    }

    #[test]
    fn create_without_flags_is_loopback_with_udp_webrtc_gossip() {
        let resolved = resolve(&[], None, Vec::new(), &[]).expect("no flags resolves");
        assert_eq!(
            resolved.lookups,
            LookupOpts::loopback(),
            "the default lookups are the loopback set"
        );
        assert_eq!(
            resolved.transport,
            policy(false, false),
            "the default transport is the plain policy"
        );
    }

    #[test]
    fn create_with_relay_lookup_adds_relay_transport() {
        let resolved =
            resolve(&[Lookup::Relay], None, Vec::new(), &[]).expect("relay lookup resolves");
        assert_eq!(
            resolved.transport,
            policy(false, true),
            "the webrtc policy is enabled"
        );
    }

    #[test]
    fn create_with_public_lookups_adds_relay_transport() {
        let resolved = resolve(
            &[Lookup::Mdns, Lookup::Dht, Lookup::Relay],
            None,
            Vec::new(),
            &[],
        )
        .expect("the public mix has no custom relay ladder, so no lookup conflicts");
        assert_eq!(
            resolved.transport,
            policy(false, true),
            "the webrtc policy stays enabled"
        );
    }

    #[test]
    fn create_with_lan_lookups_has_no_relay_transport() {
        let resolved = resolve(&[Lookup::Mdns, Lookup::Dht], None, Vec::new(), &[])
            .expect("the lan mix has no custom relay ladder, so no lookup conflicts");
        assert_eq!(
            resolved.transport,
            policy(false, false),
            "the udp-only policy is kept"
        );
    }

    fn resolve_flags(flags: &[&str]) -> anyhow::Result<Resolved> {
        let mut command_line = vec!["agent-gossip", "create"];
        command_line.extend_from_slice(flags);
        let cli = Cli::try_parse_from(command_line)
            .expect("every caller passes well-formed flags, so a parse error is a bug in the test");
        let Commands::Create { options } = cli.command else {
            panic!("expected Create command");
        };
        resolve(
            &options.lookups.lookup,
            None,
            options.lookups.nostr_url.clone(),
            &options.transport,
        )
    }

    #[test]
    fn custom_nostr_list_alone_turns_the_nostr_transport_on() {
        let resolved = resolve_flags(&["--lookup", "mdns,nostr", "--nostr-url", "wss://a.example"])
            .expect("a custom list with the nostr lookup resolves");
        assert!(
            resolved
                .transport
                .allows(habilis_network::protocol::Transport::Nostr),
            "with no --transport, a custom list brings the nostr transport"
        );
    }

    #[test]
    fn bare_nostr_lookup_has_no_nostr_transport() {
        let resolved =
            resolve_flags(&["--lookup", "mdns,nostr"]).expect("the pinned list resolves");
        assert!(
            !resolved
                .transport
                .allows(habilis_network::protocol::Transport::Nostr),
            "the pinned relays carry lookup and signalling, not payload"
        );
    }

    #[test]
    fn nostr_transport_without_a_custom_list_is_the_engine_error() {
        let error = resolve_flags(&["--lookup", "nostr", "--transport", "udp,nostr"])
            .err()
            .expect("the nostr transport needs --nostr-url");
        assert!(
            error.to_string().contains("nostr"),
            "the engine error names nostr: {error}"
        );
    }

    #[test]
    fn nostr_transport_without_the_nostr_lookup_is_an_error() {
        assert!(
            resolve_flags(&[
                "--lookup",
                "mdns",
                "--transport",
                "udp,nostr",
                "--nostr-url",
                "wss://a.example"
            ])
            .is_err(),
            "the nostr transport and a list need `nostr` in --lookup"
        );
    }

    #[test]
    fn a_nostr_url_list_changes_the_mesh_id() {
        let id = |flags: &[&str]| {
            let resolved = resolve_flags(flags).expect("these flags resolve");
            Mesh::for_topic(
                "standup",
                MeshConfig::new(resolved.lookups, None, None, resolved.transport),
            )
            .expect("the config is valid")
            .to_string()
        };
        let pinned = id(&["--lookup", "mdns,nostr"]);
        let custom = id(&["--lookup", "mdns,nostr", "--nostr-url", "wss://a.example"]);
        let other = id(&["--lookup", "mdns,nostr", "--nostr-url", "wss://b.example"]);
        assert_ne!(pinned, custom, "a custom list moves the id");
        assert_ne!(custom, other, "another list moves it again");
    }

    #[test]
    fn explicit_transport_list_is_literal() {
        let resolved = resolve_flags(&["--lookup", "relay", "--transport", "udp,webrtc,gossip"])
            .expect("an explicit list resolves");
        assert_eq!(
            resolved.transport,
            policy(false, false),
            "the plain policy is kept with no transport flag"
        );
    }

    #[test]
    fn explicit_transport_with_relay_names_the_relay() {
        let resolved = resolve_flags(&[
            "--lookup",
            "relay",
            "--transport",
            "udp,webrtc,gossip,relay",
        ])
        .expect("an explicit list with relay resolves");
        assert_eq!(
            resolved.transport,
            policy(false, true),
            "the webrtc policy is kept when given"
        );
    }

    #[test]
    fn create_advertise_without_lookup_is_public_with_relay_transport() {
        let cli = Cli::try_parse_from(["agent-gossip", "create", "--advertise"])
            .expect("a bare --advertise parses");
        let Commands::Create { options } = cli.command else {
            panic!("expected Create command");
        };
        let lookups = lookups_or_public(&options.lookups.lookup, options.advertise.is_some());
        let resolved =
            resolve(&lookups, None, Vec::new(), &options.transport).expect("advertise resolves");
        assert_eq!(
            resolved.lookups,
            LookupOpts::public_preset(),
            "the public preset supplies the public lookups"
        );
        assert_eq!(
            resolved.transport,
            policy(false, true),
            "the public preset keeps the webrtc policy"
        );
    }

    #[test]
    fn advertise_keeps_the_lookups_that_are_named() {
        assert_eq!(
            lookups_or_public(&[Lookup::Mdns], true),
            [Lookup::Mdns],
            "--public keeps the mdns lookup as given"
        );
        assert!(
            lookups_or_public(&[], false).is_empty(),
            "without lookups the public set is empty"
        );
    }

    #[test]
    fn transport_multihop_turns_multihop_on() {
        let resolved = resolve_flags(&["--transport", "udp,webrtc,multihop"])
            .expect("multihop with direct paths resolves");
        assert_eq!(
            resolved.transport,
            TransportPolicy::default().gossip(false),
            "multihop with direct paths gives the multihop policy"
        );
    }

    #[test]
    fn transport_multihop_alone_errors() {
        assert!(
            resolve_flags(&["--transport", "multihop"]).is_err(),
            "a multihop transport is rejected"
        );
    }

    #[test]
    fn transport_relay_needs_relay_lookup() {
        assert!(
            resolve_flags(&["--transport", "udp,webrtc,relay"]).is_err(),
            "a relay transport with udp and webrtc is rejected"
        );
    }

    #[test]
    fn transport_p2p_is_rejected() {
        assert!(
            Cli::try_parse_from(["agent-gossip", "create", "--transport", "p2p"]).is_err(),
            "an unknown transport name is rejected"
        );
        assert!(
            Cli::try_parse_from(["agent-gossip", "create", "--transport", "p2p,relay"]).is_err(),
            "a p2p,relay list is rejected"
        );
    }

    #[test]
    fn relay_transport_with_relay_lookup_resolves() {
        let relay = [Transport::Udp, Transport::WebRtc, Transport::Relay];
        let Ok(resolved) = resolve(&[Lookup::Relay], None, Vec::new(), &relay) else {
            panic!("relay lookup + relay transport must resolve");
        };
        assert!(
            resolved
                .transport
                .allows(habilis_network::protocol::Transport::Relay),
            "a relay transport flag enables the relay transport"
        );
        assert_eq!(
            resolved.lookups.relay_lookup,
            RelayChoice::Pinned,
            "a pinned relay lookup is recorded"
        );
    }
}
