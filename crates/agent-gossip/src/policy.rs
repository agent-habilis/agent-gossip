use habilis_network::protocol::{LookupSet, TransportPolicy};

/// The transport a create takes when it names none: every direct path and
/// gossip, with the relay carrying payload whenever the relay is a lookup.
/// Multihop is off because it stands up a second endpoint on every native
/// member; a mesh that wants it names it in `--transport`.
pub(crate) fn app_transport_policy(lookups: &LookupSet) -> TransportPolicy {
    app_policy(lookups.relay_lookup.is_set())
}

/// [`app_transport_policy`] for a caller that already knows whether the relay
/// is a lookup.
pub(crate) fn app_policy(relay_lookup: bool) -> TransportPolicy {
    TransportPolicy {
        udp: true,
        webrtc: true,
        multihop: false,
        gossip: true,
        relay_transport: relay_lookup,
    }
}

#[cfg(test)]
mod tests {
    use habilis_network::protocol::{LookupSet, RelaySelection};

    use super::{app_policy, app_transport_policy};

    #[test]
    fn loopback_lookups_carry_no_relay() {
        let policy = app_transport_policy(&LookupSet::default());
        assert_eq!(policy, app_policy(false));
        assert!(policy.udp && policy.webrtc && policy.gossip);
        assert!(!policy.multihop && !policy.relay_transport);
    }

    #[test]
    fn a_relay_lookup_carries_the_relay() {
        let lookups = LookupSet {
            relay_lookup: RelaySelection::Default,
            ..LookupSet::default()
        };
        assert_eq!(app_transport_policy(&lookups), app_policy(true));
    }

    #[test]
    fn mdns_and_dht_alone_carry_no_relay() {
        let lookups = LookupSet {
            mdns: true,
            dht: true,
            ..LookupSet::default()
        };
        assert!(!app_transport_policy(&lookups).relay_transport);
    }
}
