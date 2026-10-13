use anyhow::Result;
use habilis_network::protocol::{
    LookupSet, Mesh, MeshConfig, Nickname, RelaySelection, resolve_lookups,
};
use habilis_network::runtime::{Resolved, SetupKind, derive_topic_mesh_config};
use habilis_network::util::tuning::topic_mdns_only_for_test;

use crate::policy::app_transport_policy;

// Both lanes take the app transport policy, never the engine default (which
// has multihop on). The policy is baked into the topic id, so peers on another
// policy land in a different gossip.
pub(crate) fn derive_topic_mesh(string: &str) -> Result<Mesh> {
    // The test lane reaches no further than the LAN and has no relay lookup,
    // so the policy gives it no relay transport.
    let lookups = if topic_mdns_only_for_test() {
        LookupSet {
            mdns: true,
            dht: false,
            relay_lookup: RelaySelection::Unset,
        }
    } else {
        LookupSet {
            mdns: true,
            dht: true,
            relay_lookup: RelaySelection::Default,
        }
    };
    derive_topic_mesh_config(
        string,
        MeshConfig {
            transport: app_transport_policy(&lookups),
            lookups: resolve_lookups(lookups),
            password: None,
            issuer_pubkey: None,
        },
    )
}

pub(crate) fn resolve_topic(string: String, nickname: Option<Nickname>) -> Result<Resolved> {
    let mesh = derive_topic_mesh(&string)?;
    Ok(Resolved {
        kind: SetupKind::Topic {
            mesh,
            topic_string: string,
        },
        author: nickname.unwrap_or_else(Nickname::random),
        advertise_directory: None,
    })
}

#[cfg(test)]
mod tests {
    use habilis_network::protocol::{LookupOpts, TransportPolicy};

    use super::derive_topic_mesh;

    /// The id that habilis-network 2b468bb (mesh id v2) minted for `standup` with
    /// the policy `udp,webrtc,gossip,relay`. It is unchanged at 92a332a. It is a
    /// deliberate split from the 0.11.2 id. A drift here splits a topic into two
    /// gossips, one per version.
    #[test]
    fn topic_id_matches_the_released_derivation() {
        let mesh = derive_topic_mesh("standup").unwrap();
        assert_eq!(
            mesh.to_string(),
            "L4rA2igekqYAiecUqXZpt3yPPEPPUwDF6UuaVXxT4QdHbsiKQHCcvAJbq37HwwmKbD"
        );
    }

    #[test]
    fn topic_mesh_is_public_with_udp_webrtc_gossip_and_relay_but_no_multihop() {
        let mesh = derive_topic_mesh("x").expect("a non-empty string derives");
        assert_eq!(mesh.config.lookups, LookupOpts::public_preset());
        assert_eq!(
            mesh.config.transport,
            TransportPolicy {
                udp: true,
                webrtc: true,
                multihop: false,
                gossip: true,
                relay_transport: true,
            }
        );
    }
}
