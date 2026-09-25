use anyhow::Result;
use fofoca::protocol::{LookupOpts, Mesh, MeshConfig, Nickname, TransportPolicy};
use fofoca::runtime::{Resolved, SetupKind, derive_topic_mesh_config};
use fofoca::util::tuning::topic_mdns_only_for_test;

// The engine's topic preset is p2p-only; a public gossip here also lets the
// relay carry payload, so two peers behind strict NATs still exchange. The
// transport is baked into the topic id, so peers on a p2p-only build of the
// same string land in a different gossip.
pub(crate) fn derive_topic_mesh(string: &str) -> Result<Mesh> {
    // The test lane has no relay lookup, and relay transport requires one.
    if topic_mdns_only_for_test() {
        return fofoca::runtime::derive_topic_mesh(string);
    }
    derive_topic_mesh_config(
        string,
        MeshConfig {
            lookups: LookupOpts::public_preset(),
            password: None,
            issuer_pubkey: None,
            transport: TransportPolicy {
                relay_transport: true,
            },
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
    use fofoca::protocol::LookupOpts;

    use super::derive_topic_mesh;

    #[test]
    fn topic_mesh_uses_relay_transport() {
        let mesh = derive_topic_mesh("x").expect("a non-empty string derives");
        assert_eq!(mesh.config.lookups, LookupOpts::public_preset());
        assert!(mesh.config.transport.relay_transport);
    }
}
