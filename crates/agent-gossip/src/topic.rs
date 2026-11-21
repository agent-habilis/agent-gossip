use anyhow::Result;
use habilis_network::protocol::{
    LookupSet, Mesh, MeshConfig, Nickname, RelaySelection, resolve_lookups,
};
use habilis_network::runtime::{Resolved, SetupKind, TopicParams, derive_topic_mesh_config};
use habilis_network::util::tuning;

use crate::policy;

// Both lanes take the app transport policy. The policy is baked into the topic
// id, so peers on another policy land in a different gossip.
pub(crate) fn derive_topic_mesh(string: &str) -> Result<Mesh> {
    // The test lane reaches no further than the LAN and has no relay lookup,
    // so the policy gives it no relay transport.
    let mut lookups = LookupSet::default();
    lookups.mdns = true;
    if !tuning::topic_mdns_only_for_test() {
        lookups.dht = true;
        lookups.relay_lookup = RelaySelection::Default;
    }
    let transport = policy::app_transport_policy(&lookups);
    let config = MeshConfig::new(resolve_lookups(lookups), None, None, transport);
    derive_topic_mesh_config(string, config).map_err(anyhow::Error::from)
}

pub(crate) fn resolve_topic(string: String, nickname: Option<Nickname>) -> Result<Resolved> {
    let mesh = derive_topic_mesh(&string)?;
    // `Resolved` has no constructor, and `TopicParams::resolve` derives the engine's default
    // transport policy, so the app's mesh replaces the derived kind.
    let mut params = TopicParams::default();
    params.string.clone_from(&string);
    params.nickname = nickname;
    let mut resolved = params.resolve()?;
    resolved.kind = SetupKind::Topic {
        mesh,
        topic_string: string,
    };
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use habilis_network::protocol::{LookupOpts, TransportPolicy};

    /// The id that habilis-network 99f2ece minted for `standup` with the policy
    /// `udp,webrtc,multihop,relay`. It is a deliberate split from the id of the
    /// release before the multihop default. A drift here splits a topic into two
    /// gossips, one per version.
    #[test]
    fn topic_id_matches_the_released_derivation() {
        let mesh = super::derive_topic_mesh("standup").expect("the test topic is valid");
        assert_eq!(
            mesh.to_string(),
            "L4rA2igekqYAiecUqXZpt3yPPEPPUwDF6UuaVXxT4QdHbsiKQHCcvAJbq37GbrSCT6",
            "the topic id derivation matches the released value"
        );
    }

    #[test]
    fn topic_mesh_is_public_with_udp_webrtc_multihop_and_relay_but_no_gossip() {
        let mesh = super::derive_topic_mesh("x").expect("a non-empty string derives");
        assert_eq!(
            mesh.config().lookups,
            LookupOpts::public_preset(),
            "a topic mesh uses the public lookups"
        );
        assert_eq!(
            mesh.config().transport,
            TransportPolicy::default().relay_transport(true),
            "a topic has no creator to dial, so relay carries peers that cannot connect directly"
        );
    }
}
