use std::collections::HashSet;

use habilis_network::protocol::{AuthorKey, Nickname};
use habilis_network::x25519_dalek::PublicKey;

use super::{
    AgentCapabilities, AgentCard, AgentExtension, AgentInterface, AgentSkill, EXT_MESH_A2A_RPC,
    EXT_MESH_BLOB, EXT_MESH_BROADCAST, EXT_MESH_SEAL, EXT_MESH_STATE, GOSSIP_BINDING,
    PROTOCOL_VERSION,
};

/// The bare-`<pubkey>` peer address that carries a member's Ed25519 identity in
/// its `AgentInterface`. A2A v1.0 requires every card to declare ≥1 reachable
/// interface with a `url`; a mesh peer has no HTTP endpoint, so its gossip
/// interface is addressed by public key alone.
#[must_use]
pub(crate) fn peer_url(author_key: AuthorKey) -> String {
    author_key.to_string()
}

/// The card of another member, built from what this node knows of it: its nickname and its author
/// key. Nothing comes from the peer, so the skills, the description and the version are the ones of
/// this daemon. `relay_url` is the address at the local binding that relays JSON-RPC to the member
/// over gossip. `seal` is the key that the engine seals a directed body to; with no key the card
/// leaves the `mesh-seal` extension out, so it never names a key that is not known.
#[must_use]
pub(crate) fn peer_card(
    asked: AuthorKey,
    nickname: &Nickname,
    seal: Option<&PublicKey>,
    relay_url: &str,
) -> AgentCard {
    let seal_b58 = seal.map(|key| bs58::encode(key.as_bytes()).into_string());
    let mut card = full_card(nickname, asked, seal_b58.as_deref().unwrap_or_default());
    if seal_b58.is_none() {
        card.capabilities
            .extensions
            .retain(|extension| extension.uri != EXT_MESH_SEAL);
    }
    // The member has no HTTP endpoint of its own: this daemon relays JSON-RPC to it, so that a
    // `SendMessage` at the relay address creates a task on the member.
    card.supported_interfaces.push(AgentInterface {
        url: relay_url.to_owned(),
        protocol_binding: "JSONRPC".to_owned(),
        tenant: None,
        protocol_version: PROTOCOL_VERSION.to_owned(),
    });
    card
}

/// The card with every optional protocol field filled in.
#[must_use]
pub(crate) fn full_card(
    nickname: &Nickname,
    author_key: AuthorKey,
    seal_pubkey_b58: &str,
) -> AgentCard {
    let extension = |uri: &str, description: &str| AgentExtension {
        uri: uri.to_string(),
        description: Some(description.to_string()),
        required: None,
        params: None,
    };
    let modes: Vec<String> = ["text/plain", "application/json", "application/octet-stream"]
        .into_iter()
        .map(String::from)
        .collect();
    AgentCard {
        name: nickname.as_str().to_string(),
        description: format!(
            "agent-gossip peer `{nickname}` — an AI agent reachable over the \
             mesh's A2A gossip binding"
        ),
        supported_interfaces: vec![AgentInterface {
            url: peer_url(author_key),
            protocol_binding: GOSSIP_BINDING.to_string(),
            tenant: None,
            protocol_version: PROTOCOL_VERSION.to_string(),
        }],
        version: crate::version().to_string(),
        documentation_url: None,
        capabilities: AgentCapabilities {
            streaming: Some(true),
            push_notifications: Some(false),
            extended_agent_card: Some(true),
            extensions: vec![
                extension(
                    EXT_MESH_BROADCAST,
                    "gossip-wide broadcast Messages (A2A is point-to-point; a broadcast declares itself)",
                ),
                extension(
                    EXT_MESH_STATE,
                    "a shared RFC 7386 JSON document per gossip (state/meta channels)",
                ),
                extension(
                    EXT_MESH_A2A_RPC,
                    "serves A2A over gossip (request/response and request/stream): reads, task cancel, task creation, and streaming subscriptions directed at this member",
                ),
                extension(
                    EXT_MESH_BLOB,
                    "large files on a Part travel as a url reference (a ticket), fetched point-to-point over a dedicated QUIC channel and SHA-256-verified, instead of inlining over gossip",
                ),
                AgentExtension {
                    uri: EXT_MESH_SEAL.to_string(),
                    description: Some(
                        "directed frames (those addressed with a `to`) are sealed to this X25519 key; relays forward + verify the signature but cannot read the body. Broadcast stays public.".to_string(),
                    ),
                    required: None,
                    params: Some(serde_json::json!({ "x25519": seal_pubkey_b58 })),
                },
            ],
        },
        default_input_modes: modes.clone(),
        default_output_modes: modes,
        skills: vec![
            AgentSkill {
                id: "chat".to_string(),
                name: "chat".to_string(),
                description: "converse over gossip broadcast messages".to_string(),
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

/// Read the published X25519 sealing key of the member `peer` from the derived meta
/// document — the `mesh-seal` extension's `params.x25519` (base58). The card no longer lives in
/// meta, so this finds nothing until the engine seals to the key of the addressee itself and this
/// reader goes.
pub(crate) fn peer_seal_key(meta_doc: &serde_json::Value, peer: &AuthorKey) -> Option<PublicKey> {
    let extensions = meta_doc
        .pointer(&format!("/peers/{peer}/card/capabilities/extensions"))?
        .as_array()?;
    let b58: &str = extensions
        .iter()
        .find(|ext| {
            ext.get("uri")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|uri| uri.contains("mesh-seal"))
        })?
        .pointer("/params/x25519")?
        .as_str()?;
    let bytes: [u8; 32] = bs58::decode(b58).into_vec().ok()?.try_into().ok()?;
    Some(PublicKey::from(bytes))
}

/// The author keys in the `meta` view with no **active** member behind them.
///
/// The view maps the hex of each author key to its slice. A member that leaves gracefully sends a
/// tombstone, and the engine drops its slice. A member that dies without a goodbye keeps its slice
/// until the other members drop its quiet key, about 1 hour, still reporting whatever status its
/// agent last set. Such a slice is
/// history, not a peer to give work to.
///
/// `live` must hold only the *active* peers. A peer that dies ungracefully is moved to the
/// roster's quiet set rather than dropped, and the roster snapshot returns both, so passing the
/// snapshot wholesale reports nothing. Being listed while merely quiet is the honest answer either
/// way: quiet means no live link, which is what a caller about to delegate work needs to know.
///
/// `own` is excluded because the roster lists peers, not self: without it every node would report
/// its own slice as absent. A slot is the hex of the author key, because two members can share a
/// nickname.
#[must_use]
pub(crate) fn absent_peers<'view>(
    view: &'view serde_json::Value,
    live: &HashSet<AuthorKey>,
    own: &AuthorKey,
) -> Vec<&'view str> {
    let is_absent =
        |slot: &&str| !own.matches_hex(slot) && !live.iter().any(|key| key.matches_hex(slot));
    view.as_object()
        .map(|slices| {
            slices
                .keys()
                .map(String::as_str)
                .filter(is_absent)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use habilis_network::protocol::{AuthorKey, Nickname};
    use habilis_network::x25519_dalek::PublicKey;

    use super::peer_card;

    fn key() -> AuthorKey {
        AuthorKey::from([0xab; 32])
    }

    fn slot() -> String {
        key().to_string()
    }

    #[test]
    fn a_peer_card_declares_the_mesh_extensions_and_identity() {
        let seal = PublicKey::from([7u8; 32]);
        let card = peer_card(
            key(),
            &Nickname::from("calm-otter"),
            Some(&seal),
            "http://127.0.0.1:1/peers/ab",
        );
        assert_eq!(
            card.name, "calm-otter",
            "the card names the calm-otter peer"
        );
        assert_eq!(
            card.capabilities.streaming,
            Some(true),
            "the card advertises streaming"
        );
        let uris: Vec<&str> = card
            .capabilities
            .extensions
            .iter()
            .map(|ext| ext.uri.as_str())
            .collect();
        for needle in [
            "mesh-broadcast",
            "mesh-state",
            "mesh-a2a-rpc",
            "mesh-blob",
            "mesh-seal",
        ] {
            assert!(
                uris.iter().any(|uri| uri.contains(needle)),
                "card must declare {needle}"
            );
        }
        let seal_ext = card
            .capabilities
            .extensions
            .iter()
            .find(|ext| ext.uri.contains("mesh-seal"))
            .expect("the card declares the mesh-seal extension when it has a key");
        assert_eq!(
            seal_ext
                .params
                .as_ref()
                .expect("the mesh-seal extension always carries params")["x25519"],
            bs58::encode([7u8; 32]).into_string(),
            "the seal extension carries the x25519 key"
        );
    }

    #[test]
    fn a_peer_card_has_the_gossip_interface_and_the_relay_interface() {
        let card = peer_card(
            key(),
            &Nickname::from("calm-otter"),
            None,
            "http://127.0.0.1:1/peers/ab",
        );
        assert_eq!(
            card.supported_interfaces.len(),
            2,
            "the card lists the gossip interface and the relay interface"
        );
        assert_eq!(
            card.supported_interfaces[0].url,
            "ab".repeat(32),
            "the first interface url is the author key"
        );
        assert_eq!(
            card.supported_interfaces[1].url, "http://127.0.0.1:1/peers/ab",
            "the second interface is the relay address"
        );
        assert_eq!(
            card.supported_interfaces[1].protocol_binding, "JSONRPC",
            "the relay interface speaks JSON-RPC"
        );
    }

    #[test]
    fn a_peer_card_with_no_seal_key_leaves_the_seal_extension_out() {
        let card = peer_card(key(), &Nickname::from("calm-otter"), None, "http://x/");
        assert!(
            card.capabilities
                .extensions
                .iter()
                .all(|ext| !ext.uri.contains("mesh-seal")),
            "a card never names a seal key that is not known"
        );
    }

    /// A peer that died without a graceful leave keeps its meta slice until the gone mark ends,
    /// still carrying whatever status it last set, which is what a peer-picking agent goes on.
    #[test]
    fn absent_peers_names_slices_with_no_live_member() {
        let (live_key, gone_key, own_key) = (
            AuthorKey::from([1; 32]),
            AuthorKey::from([2; 32]),
            AuthorKey::from([3; 32]),
        );
        let view = serde_json::json!({
            live_key.to_string(): { "status": "idle" },
            gone_key.to_string(): { "status": "idle" },
            own_key.to_string(): { "status": "busy" },
        });
        let live = HashSet::from([live_key]);

        let absent = super::absent_peers(&view, &live, &own_key);

        assert_eq!(
            absent,
            vec![gone_key.to_string().as_str()],
            "only the slice with no live peer behind it"
        );
    }

    /// The roster lists peers, never self, so our own slice has to be excluded explicitly.
    #[test]
    fn absent_peers_never_includes_our_own_slice() {
        let view = serde_json::json!({ slot(): { "status": "idle" } });
        assert!(
            super::absent_peers(&view, &HashSet::new(), &key()).is_empty(),
            "our own slice is not reported absent"
        );
    }

    /// A view with no slice at all (a fresh mesh) is not an error.
    #[test]
    fn absent_peers_tolerates_an_empty_view() {
        assert!(
            super::absent_peers(&serde_json::json!({}), &HashSet::new(), &key()).is_empty(),
            "an empty view has no absent peers"
        );
    }
}
