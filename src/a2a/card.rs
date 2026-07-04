use iroh::{Endpoint, EndpointAddr, EndpointId};

use crate::protocol::nickname::Nickname;

use super::{
    AgentCapabilities, AgentCard, AgentExtension, AgentInterface, AgentSkill, EXT_SWARM_A2A_RPC,
    EXT_SWARM_BLOB, EXT_SWARM_BROADCAST, EXT_SWARM_SEAL, EXT_SWARM_STATE, GOSSIP_BINDING,
    PROTOCOL_VERSION,
};

/// The `swarm+gossip://` URL scheme that carries a member's Ed25519 identity in
/// its `AgentInterface`. A2A v1.0 requires every card to declare ≥1 reachable
/// interface with a `url`; a mesh peer has no HTTP endpoint, so its gossip
/// interface is addressed by public key.
#[must_use]
pub(crate) fn gossip_url(pubkey_hex: &str) -> String {
    format!("swarm+gossip://{pubkey_hex}")
}

/// This participant's `AgentCard` — the canonical A2A self-description every
/// member publishes into the meta channel at `/peers/<nick>/card` on join.
/// Peers enumerate each other's cards from the meta document, so discovery
/// works with no HTTP server at all; the (flag-gated) local JSON-RPC binding
/// additionally serves the card at `/.well-known/agent-card.json` (with an
/// extra `JSONRPC` interface).
///
/// The identity is the Ed25519 **public key**, carried in the gossip
/// `AgentInterface.url` (`swarm+gossip://<pubkey>`), not the display nickname;
/// `capabilities.extensions` declares the swarm's protocol extensions so a
/// strict A2A client can gate on them. Agent-side facts the daemon cannot know
/// (model, harness, host, extra skills) are merged by the agent as sibling keys
/// under `/peers/<nick>` — the card is the daemon's contribution, not the whole
/// peer entry.
#[must_use]
pub(crate) fn own_card(nickname: &Nickname, pubkey_hex: &str, seal_pubkey_b58: &str) -> AgentCard {
    let extension = |uri: &str, description: &str| AgentExtension {
        uri: uri.to_string(),
        description: Some(description.to_string()),
        required: None,
        params: None,
    };
    AgentCard {
        name: nickname.as_str().to_string(),
        description: format!(
            "agent-gossip participant `{nickname}` — an AI agent reachable over the \
             swarm's A2A gossip binding"
        ),
        supported_interfaces: vec![AgentInterface {
            url: gossip_url(pubkey_hex),
            protocol_binding: GOSSIP_BINDING.to_string(),
            tenant: None,
            protocol_version: PROTOCOL_VERSION.to_string(),
        }],
        version: crate::VERSION.to_string(),
        documentation_url: None,
        capabilities: AgentCapabilities {
            streaming: Some(true),
            push_notifications: Some(false),
            extended_agent_card: Some(true),
            extensions: vec![
                extension(
                    EXT_SWARM_BROADCAST,
                    "swarm-wide broadcast Messages (A2A is point-to-point; a broadcast declares itself)",
                ),
                extension(
                    EXT_SWARM_STATE,
                    "a shared RFC 7386 JSON document per swarm (state/meta channels)",
                ),
                extension(
                    EXT_SWARM_A2A_RPC,
                    "serves A2A over gossip (request/response and request/stream): reads, task cancel, task creation, and streaming subscriptions directed at this member",
                ),
                extension(
                    EXT_SWARM_BLOB,
                    "large files on a Part travel as a url reference (a 💬 ticket), fetched point-to-point over a dedicated QUIC channel and SHA-256-verified, instead of inlining over gossip",
                ),
                AgentExtension {
                    uri: EXT_SWARM_SEAL.to_string(),
                    description: Some(
                        "directed frames (those addressed with a `to`) are sealed to this X25519 key; relays forward + verify the signature but cannot read the body. Broadcast stays public.".to_string(),
                    ),
                    required: None,
                    params: Some(serde_json::json!({ "x25519": seal_pubkey_b58 })),
                },
            ],
        },
        default_input_modes: vec![
            "text/plain".to_string(),
            "application/json".to_string(),
            "application/octet-stream".to_string(),
        ],
        default_output_modes: vec![
            "text/plain".to_string(),
            "application/json".to_string(),
            "application/octet-stream".to_string(),
        ],
        skills: vec![
            AgentSkill {
                id: "chat".to_string(),
                name: "chat".to_string(),
                description: "converse over swarm broadcast messages".to_string(),
                tags: vec!["chat".to_string()],
                examples: Vec::new(),
                input_modes: Vec::new(),
                output_modes: Vec::new(),
            },
            AgentSkill {
                id: "delegate".to_string(),
                name: "delegate".to_string(),
                description: "accept a delegated task and return the result as an artifact"
                    .to_string(),
                tags: vec!["task".to_string()],
                examples: Vec::new(),
                input_modes: Vec::new(),
                output_modes: Vec::new(),
            },
        ],
        security_schemes: None,
    }
}

/// Read peer `<nick>`'s published X25519 sealing key from the derived meta
/// document — the `swarm-seal` extension's `params.x25519` (base58). `None` if
/// the peer's card or key hasn't replicated yet. The card is bound to its author
/// (foreign-card merges are dropped), so this key is authenticated by the same
/// forgery protection as the rest of the card.
pub(crate) fn peer_seal_key(meta_doc: &serde_json::Value, peer: &Nickname) -> Option<[u8; 32]> {
    let extensions = meta_doc
        .pointer(&format!("/peers/{peer}/card/capabilities/extensions"))?
        .as_array()?;
    let b58 = extensions
        .iter()
        .find(|ext| {
            ext.get("uri")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|uri| uri.contains("swarm-seal"))
        })?
        .pointer("/params/x25519")?
        .as_str()?;
    bs58::decode(b58).into_vec().ok()?.try_into().ok()
}

/// The stable dial hint published *inside* the card at
/// `/peers/<nick>/card/endpoint`: the member's transport `EndpointId` and home
/// relay. The volatile direct IPs are omitted — iroh re-holepunches from the
/// relay — so this only re-publishes when the relay re-homes, not on every path
/// change. Living inside the card, it is covered by the same forgery gate as the
/// seal key (a sibling `/peers/<nick>/addr` would be forgeable by any peer). It
/// **supplements** gossiped `PeerInfo`: a peer that has synced the meta doc can
/// dial without waiting for a `PeerInfo` to arrive.
#[must_use]
pub(crate) fn endpoint_hint(endpoint: &Endpoint) -> serde_json::Value {
    let addr = endpoint.addr();
    serde_json::json!({
        "id": addr.id.to_string(),
        "relay": addr.relay_urls().next().map(ToString::to_string),
    })
}

/// Read peer `<nick>`'s published endpoint hint from the derived meta document →
/// its `EndpointId` and a bare `EndpointAddr` (id + relay). `None` if the card or
/// hint hasn't replicated yet. Authenticated by the card-forgery gate, exactly
/// like [`peer_seal_key`].
pub(crate) fn peer_endpoint(
    meta_doc: &serde_json::Value,
    peer: &Nickname,
) -> Option<(EndpointId, EndpointAddr)> {
    let hint = meta_doc.pointer(&format!("/peers/{peer}/card/endpoint"))?;
    crate::protocol::peer_addr::endpoint_addr_from_json(hint).ok()
}

/// The RFC 7386 merge that publishes `card` at `/peers/<nick>/card`.
#[must_use]
pub(crate) fn publish_merge(nickname: &Nickname, card: &AgentCard) -> serde_json::Value {
    serde_json::json!({
        "peers": {
            nickname.as_str(): { "card": card }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{own_card, publish_merge};
    use crate::protocol::Nickname;

    #[test]
    fn card_declares_the_swarm_extensions_and_identity() {
        let seal_b58 = bs58::encode([7u8; 32]).into_string();
        let card = own_card(&Nickname::from("calm-otter"), &"ab".repeat(32), &seal_b58);
        assert_eq!(card.name, "calm-otter");
        assert_eq!(card.capabilities.streaming, Some(true));
        let uris: Vec<&str> = card
            .capabilities
            .extensions
            .iter()
            .map(|ext| ext.uri.as_str())
            .collect();
        for needle in [
            "swarm-broadcast",
            "swarm-state",
            "swarm-a2a-rpc",
            "swarm-blob",
            "swarm-seal",
        ] {
            assert!(
                uris.iter().any(|uri| uri.contains(needle)),
                "card must declare {needle}"
            );
        }
        // The seal extension carries this member's X25519 public key.
        let seal_ext = card
            .capabilities
            .extensions
            .iter()
            .find(|ext| ext.uri.contains("swarm-seal"))
            .expect("swarm-seal extension present");
        assert_eq!(seal_ext.params.as_ref().unwrap()["x25519"], seal_b58);
        // The mesh card's only interface is the gossip binding, whose url
        // carries the cryptographic identity (no HTTP url).
        assert_eq!(card.supported_interfaces.len(), 1);
        assert_eq!(
            card.supported_interfaces[0].url,
            format!("swarm+gossip://{}", "ab".repeat(32)),
        );
    }

    #[test]
    fn publish_merge_targets_the_peer_card_path() {
        let nickname = Nickname::from("calm-otter");
        let seal_b58 = bs58::encode([7u8; 32]).into_string();
        let card = own_card(&nickname, &"ab".repeat(32), &seal_b58);
        let merge = publish_merge(&nickname, &card);
        assert_eq!(merge["peers"]["calm-otter"]["card"]["name"], "calm-otter");
        assert!(
            merge["peers"]["calm-otter"].get("model").is_none(),
            "agent-side facts are the agent's own merge, not the daemon's"
        );
    }
}
