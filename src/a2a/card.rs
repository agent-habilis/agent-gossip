use crate::protocol::nickname::Nickname;

use super::{
    AgentCapabilities, AgentCard, AgentExtension, AgentInterface, AgentSkill, EXT_SWARM_A2A_RPC,
    EXT_SWARM_BROADCAST, EXT_SWARM_STATE, GOSSIP_BINDING, PROTOCOL_VERSION,
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
pub(crate) fn own_card(nickname: &Nickname, pubkey_hex: &str) -> AgentCard {
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
            ],
        },
        default_input_modes: vec!["text/plain".to_string(), "application/json".to_string()],
        default_output_modes: vec!["text/plain".to_string(), "application/json".to_string()],
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
        let card = own_card(&Nickname::from("calm-otter"), &"ab".repeat(32));
        assert_eq!(card.name, "calm-otter");
        assert_eq!(card.capabilities.streaming, Some(true));
        let uris: Vec<&str> = card
            .capabilities
            .extensions
            .iter()
            .map(|ext| ext.uri.as_str())
            .collect();
        for needle in ["swarm-broadcast", "swarm-state", "swarm-a2a-rpc"] {
            assert!(
                uris.iter().any(|uri| uri.contains(needle)),
                "card must declare {needle}"
            );
        }
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
        let card = own_card(&nickname, &"ab".repeat(32));
        let merge = publish_merge(&nickname, &card);
        assert_eq!(merge["peers"]["calm-otter"]["card"]["name"], "calm-otter");
        assert!(
            merge["peers"]["calm-otter"].get("model").is_none(),
            "agent-side facts are the agent's own merge, not the daemon's"
        );
    }
}
