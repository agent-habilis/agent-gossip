use habilis_network::protocol::{
    LookupOpts, LookupSet, RelayChoice, RelayLadder, TransportPolicy, resolve_lookups,
};

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
/// # Errors
/// `relay_url` is given without `relay` in `lookups`, `transports` is invalid
/// (see [`transport_policy`]), or `transports` names `relay` while no relay
/// lookup is on.
pub(crate) fn resolve(
    lookups: &[Lookup],
    relay_url: Option<RelayLadder>,
    transports: &[Transport],
) -> anyhow::Result<Resolved> {
    let set = lookup_set(lookups, relay_url)?;
    let resolved = resolve_lookups(set.clone());
    let transport = if transports.is_empty() {
        policy::app_transport_policy(&set)
    } else {
        transport_policy(transports)?
    };
    if transport.relay_transport && resolved.relay_lookup == RelayChoice::Disabled {
        anyhow::bail!("--transport relay needs a relay lookup: add `relay` to --lookup");
    }
    Ok(Resolved {
        set,
        lookups: resolved,
        transport,
    })
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use habilis_network::protocol::{LookupOpts, RelayChoice, TransportPolicy};

    use super::{Resolved, resolve};
    use crate::cli::args::lookup::{Lookup, lookups_or_public};
    use crate::cli::args::transport::Transport;
    use crate::cli::args::{Cli, Commands};

    #[test]
    fn relay_transport_without_relay_lookup_errors() {
        let relay = [Transport::Udp, Transport::WebRtc, Transport::Relay];
        assert!(
            resolve(&[Lookup::Mdns], None, &relay).is_err(),
            "an mdns lookup without a relay is rejected"
        );
    }

    fn policy(multihop: bool, relay_transport: bool) -> TransportPolicy {
        TransportPolicy::default()
            .with_multihop(multihop)
            .with_relay_transport(relay_transport)
    }

    #[test]
    fn create_without_flags_is_loopback_with_udp_webrtc_gossip() {
        let resolved = resolve(&[], None, &[]).expect("no flags resolves");
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
        let resolved = resolve(&[Lookup::Relay], None, &[]).expect("relay lookup resolves");
        assert_eq!(
            resolved.transport,
            policy(false, true),
            "the webrtc policy is enabled"
        );
    }

    #[test]
    fn create_with_public_lookups_adds_relay_transport() {
        let resolved = resolve(&[Lookup::Mdns, Lookup::Dht, Lookup::Relay], None, &[])
            .expect("public lookups resolve");
        assert_eq!(
            resolved.transport,
            policy(false, true),
            "the webrtc policy stays enabled"
        );
    }

    #[test]
    fn create_with_lan_lookups_has_no_relay_transport() {
        let resolved =
            resolve(&[Lookup::Mdns, Lookup::Dht], None, &[]).expect("lan lookups resolve");
        assert_eq!(
            resolved.transport,
            policy(false, false),
            "the udp-only policy is kept"
        );
    }

    fn resolve_flags(flags: &[&str]) -> anyhow::Result<Resolved> {
        let mut command_line = vec!["agent-gossip", "create"];
        command_line.extend_from_slice(flags);
        let cli = Cli::try_parse_from(command_line).expect("the flags parse");
        let Commands::Create { options } = cli.command else {
            panic!("expected Create command");
        };
        resolve(&options.lookups.lookup, None, &options.transport)
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
        let resolved = resolve(&lookups, None, &options.transport).expect("advertise resolves");
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
            TransportPolicy::default().with_gossip(false),
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
        let Ok(resolved) = resolve(&[Lookup::Relay], None, &relay) else {
            panic!("relay lookup + relay transport must resolve");
        };
        assert!(
            resolved.transport.relay_transport,
            "a relay transport flag enables the relay transport"
        );
        assert_eq!(
            resolved.lookups.relay_lookup,
            RelayChoice::Pinned,
            "a pinned relay lookup is recorded"
        );
    }
}
