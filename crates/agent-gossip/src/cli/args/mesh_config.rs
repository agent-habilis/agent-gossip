use fofoca::protocol::{
    LookupOpts, LookupSet, RelayChoice, RelayLadder, TransportPolicy, resolve_lookups,
};

use super::lookup::{Lookup, lookup_set};
use super::transport::{Transport, transport_policy};

pub(crate) struct Resolved {
    pub set: LookupSet,
    pub lookups: LookupOpts,
    pub transport: TransportPolicy,
}

/// Resolve the `create` flags into the parts of a mesh config. Shared by the
/// CLI and the MCP `create_gossip` tool so the two surfaces cannot drift.
///
/// The one rule that needs both concepts lives here: letting the relay carry
/// payload needs a relay to exist as a lookup. The engine checks the same rule
/// in `MeshConfig::validate`, but names its own fields there — this check runs
/// first, naming the flags.
///
/// # Errors
/// `relay_url` is given without `relay` in `lookups`, `transports` is non-empty
/// without `p2p`, or `transports` names `relay` while no relay lookup is on.
pub(crate) fn resolve(
    lookups: &[Lookup],
    relay_url: Option<RelayLadder>,
    transports: &[Transport],
) -> anyhow::Result<Resolved> {
    let set = lookup_set(lookups, relay_url)?;
    let resolved = resolve_lookups(set.clone());
    let transport = transport_policy(transports)?;
    if transport.relay_transport && resolved.relay_lookup == RelayChoice::Disabled {
        anyhow::bail!("--transport p2p,relay needs a relay lookup: add `relay` to --lookup");
    }
    Ok(Resolved {
        set,
        lookups: resolved,
        transport,
    })
}

#[cfg(test)]
mod tests {
    use fofoca::protocol::RelayChoice;

    use super::resolve;
    use crate::cli::args::lookup::Lookup;
    use crate::cli::args::transport::Transport;

    #[test]
    fn relay_transport_without_relay_lookup_errors() {
        assert!(resolve(&[Lookup::Mdns], None, &[Transport::P2p, Transport::Relay]).is_err());
    }

    #[test]
    fn relay_transport_with_relay_lookup_resolves() {
        let Ok(resolved) = resolve(&[Lookup::Relay], None, &[Transport::P2p, Transport::Relay])
        else {
            panic!("relay lookup + relay transport must resolve");
        };
        assert!(resolved.transport.relay_transport);
        assert_eq!(resolved.lookups.relay_lookup, RelayChoice::Pinned);
    }
}
