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
/// key. Nothing comes from the peer. `seal` is the key that the engine seals a directed body to; with
/// no key the card leaves the `mesh-seal` extension out, so it never names a key that is not
/// known.
#[must_use]
pub(crate) fn peer_card(
    asked: AuthorKey,
    nickname: &Nickname,
    seal: Option<&PublicKey>,
) -> AgentCard {
    let seal_b58 = seal.map(|key| bs58::encode(key.as_bytes()).into_string());
    let mut card = full_card(nickname, asked, seal_b58.as_deref().unwrap_or_default());
    if seal_b58.is_none() {
        card.capabilities
            .extensions
            .retain(|extension| extension.uri != EXT_MESH_SEAL);
    }
    card
}

/// The card with every optional protocol field filled in. Served to a client; the meta
/// document holds only [`own_card`]'s minimal form.
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
/// document — the `mesh-seal` extension's `params.x25519` (base58). `None` if
/// the peer's card or key hasn't replicated yet. The entry of a member is its slot, the hex of
/// its author key, and the card is bound to its author (foreign-card merges are dropped), so
/// this key is authenticated by the same forgery protection as the rest of the card.
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

/// What a frame can still grow by after it was measured: the dependency list and the sequence
/// number of a later change are not in the merge that was measured.
const FRAME_MARGIN: usize = 128;

/// The `/peers` entries in `document` with no **active** member behind them.
///
/// A member retracts its own entry on a graceful leave ([`retract_merge`]),
/// and that is the only way an entry ever leaves the document — the forgery
/// gate binds every author to its own subtree, so a survivor cannot tidy up
/// after a peer that was `SIGKILL`ed or lost power. Such an entry stays
/// readable forever, still reporting whatever status its agent last set.
///
/// `live` must hold only the *active* peers. A peer that dies ungracefully is
/// moved to the roster's quiet set rather than dropped, and the roster
/// snapshot returns both, so passing the snapshot wholesale reports nothing:
/// the ghost is still in the list. Being flagged while merely quiet is the
/// honest answer either way — quiet means no live link, which is precisely
/// what a caller about to delegate work needs to know.
///
/// `own` is excluded because the roster lists peers, not self: without
/// it every node would report its own card as absent. An entry is named by its slot, the hex of
/// the author key, because two members can share a nickname.
///
/// A slot that is the nickname of a live member is no ghost either. The graph skills write
/// `{"peers":{"<nickname>":{...}}}`, and a graph tool drops every slot that `absent` lists. The
/// nickname of this member is read from its own card.
#[must_use]
pub(crate) fn absent_peers<'doc>(
    document: &'doc serde_json::Value,
    live: &HashSet<AuthorKey>,
    live_names: &HashSet<&str>,
    own: &AuthorKey,
) -> Vec<&'doc str> {
    let peers: Option<&serde_json::Map<String, serde_json::Value>> =
        document.get("peers").and_then(serde_json::Value::as_object);
    let own_entry: Option<&serde_json::Value> = peers.and_then(|peers| peers.get(&own.to_string()));
    let own_card_name: Option<&serde_json::Value> =
        own_entry.and_then(|entry| entry.pointer("/card/name"));
    let own_name: Option<&str> = own_card_name.and_then(serde_json::Value::as_str);
    peers
        .map(|peers| {
            let is_absent = |slot: &&str| {
                let is_live_key = live.iter().any(|key| key.matches_hex(slot));
                let is_live_name = live_names.contains(slot) || own_name == Some(slot);
                !own.matches_hex(slot) && !is_live_key && !is_live_name
            };
            let slots = peers.keys().map(String::as_str);
            slots.filter(is_absent).collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use habilis_network::protocol::{AuthorKey, Nickname};
    use habilis_network::util::consts;

    use super::{full_card, full_from_minimal, own_card, publish_merges};

    fn key() -> AuthorKey {
        AuthorKey::from([0xab; 32])
    }

    fn slot() -> String {
        key().to_string()
    }

    #[test]
    fn card_declares_the_mesh_extensions_and_identity() {
        let seal_b58 = bs58::encode([7u8; 32]).into_string();
        let card = own_card(
            &Nickname::from("calm-otter"),
            AuthorKey::from([0xab; 32]),
            &seal_b58,
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
        // The seal extension carries this member's X25519 public key.
        let seal_ext = card
            .capabilities
            .extensions
            .iter()
            .find(|ext| ext.uri.contains("mesh-seal"))
            .expect("the card always declares the mesh-seal extension");
        assert_eq!(
            seal_ext
                .params
                .as_ref()
                .expect("the mesh-seal extension always carries params")["x25519"],
            seal_b58,
            "the seal extension carries the x25519 key"
        );
        // The mesh card's only interface is the gossip binding, whose url
        // carries the cryptographic identity (no HTTP url).
        assert_eq!(
            card.supported_interfaces.len(),
            1,
            "the card lists one supported interface"
        );
        assert_eq!(
            card.supported_interfaces[0].url,
            "ab".repeat(32),
            "the interface url is the mesh id"
        );
    }

    /// A peer that died without a graceful leave keeps its meta entry forever:
    /// the forgery gate means no survivor can retract it. Reading the document
    /// alone therefore shows a member that is gone, still carrying whatever
    /// status it last set, which is what a peer-picking agent goes on.
    #[test]
    fn absent_peers_names_entries_with_no_live_member() {
        let (live_key, gone_key, own_key) = (
            AuthorKey::from([1; 32]),
            AuthorKey::from([2; 32]),
            AuthorKey::from([3; 32]),
        );
        let document = serde_json::json!({
            "peers": {
                live_key.to_string(): { "status": "idle" },
                gone_key.to_string(): { "status": "idle" },
                own_key.to_string(): { "status": "busy" },
            }
        });
        let live = HashSet::from([live_key]);

        let absent = super::absent_peers(&document, &live, &HashSet::new(), &own_key);

        assert_eq!(
            absent,
            vec![gone_key.to_string().as_str()],
            "only the entry with no live peer behind it"
        );
    }

    /// The roster lists peers, never self, so our own entry has to be excluded
    /// explicitly. Without that, every node reports its own card as a ghost.
    #[test]
    fn absent_peers_never_includes_our_own_entry() {
        let document = serde_json::json!({ "peers": { slot(): { "status": "idle" } } });
        let live = HashSet::new();
        assert!(
            super::absent_peers(&document, &live, &HashSet::new(), &key()).is_empty(),
            "a live peer is not reported absent"
        );
    }

    /// A slot named by the nickname of a live member, or of this member, is a graph slot and not a
    /// ghost; the same slot with no live member behind the name is absent.
    #[test]
    fn a_slot_named_by_a_live_nickname_is_not_absent() {
        let own_key = AuthorKey::from([3; 32]);
        let document = serde_json::json!({
            "peers": {
                own_key.to_string(): { "card": { "name": "me" } },
                "me": { "instance": "g" },
                "bob": { "instance": "g" },
                "gone": { "instance": "g" },
            }
        });
        let live_names = HashSet::from(["bob"]);

        let absent = super::absent_peers(&document, &HashSet::new(), &live_names, &own_key);

        assert_eq!(
            absent,
            vec!["gone"],
            "only the slot of a name that no live member holds"
        );
    }

    /// A document with no `/peers` map at all (a fresh mesh) is not an error.
    #[test]
    fn absent_peers_tolerates_a_document_without_peers() {
        let live = HashSet::new();
        assert!(
            super::absent_peers(&serde_json::json!({}), &live, &HashSet::new(), &key()).is_empty(),
            "a document without peers has no absent peers"
        );
    }

    /// Two members use one nickname. Their entries are two slots, so the card, the seal key and
    /// the endpoint of one key are never read for the other.
    #[test]
    fn the_entry_of_a_key_is_not_the_entry_of_another_key_of_the_same_nickname() {
        let nickname = Nickname::from("calm-otter");
        let (first, second) = (AuthorKey::from([1; 32]), AuthorKey::from([2; 32]));
        let first_seal = bs58::encode([7u8; 32]).into_string();
        let second_seal = bs58::encode([8u8; 32]).into_string();
        let mut first_card = serde_json::to_value(own_card(&nickname, first, &first_seal))
            .expect("a test value is plain data and always serializes");
        first_card["endpoint"] = serde_json::json!({ "id": "01".repeat(32), "relay": null });
        let second_card = serde_json::to_value(own_card(&nickname, second, &second_seal))
            .expect("a test value is plain data and always serializes");
        let document = serde_json::json!({ "peers": {
            first.to_string(): { "card": first_card },
            second.to_string(): { "card": second_card },
        } });

        assert_eq!(
            super::peer_seal_key(&document, &first).map(|key| *key.as_bytes()),
            Some([7u8; 32]),
            "the seal key of the first key is its own"
        );
        assert_eq!(
            super::peer_seal_key(&document, &second).map(|key| *key.as_bytes()),
            Some([8u8; 32]),
            "the seal key of the second key is its own, not the one of the same nickname"
        );
        assert!(
            super::peer_endpoint(&document, &first).is_some()
                && super::peer_endpoint(&document, &second).is_none(),
            "only the first key published an endpoint hint"
        );
    }
}
