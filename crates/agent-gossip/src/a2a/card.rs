use std::collections::HashSet;

use habilis_network::iroh::{Endpoint, EndpointAddr, EndpointId};
use habilis_network::protocol::{AuthorKey, Nickname};
use habilis_network::util::consts::MAX_MESSAGE_SIZE;
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

/// This peer's `AgentCard` — the canonical A2A self-description every
/// member publishes into the meta channel at `/peers/<nick>/card` on join.
/// Peers enumerate each other's cards from the meta document, so discovery
/// works with no HTTP server at all; the (flag-gated) local JSON-RPC binding
/// additionally serves the card at `/.well-known/agent-card.json` (with an
/// extra `JSONRPC` interface).
///
/// The identity is the Ed25519 **public key**, carried in the gossip
/// `AgentInterface.url` (the bare `<pubkey>`), not the display nickname;
/// `capabilities.extensions` declares the mesh's protocol extensions so a
/// strict A2A client can gate on them. Agent-side facts the daemon cannot know
/// (model, harness, host, cwd, extra skills) are merged by the agent as sibling keys
/// under `/peers/<nick>` — the card is the daemon's contribution, not the whole
/// peer entry.
#[must_use]
pub(crate) fn own_card(
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

/// Read peer `<nick>`'s published X25519 sealing key from the derived meta
/// document — the `mesh-seal` extension's `params.x25519` (base58). `None` if
/// the peer's card or key hasn't replicated yet. The card is bound to its author
/// (foreign-card merges are dropped), so this key is authenticated by the same
/// forgery protection as the rest of the card.
pub(crate) fn peer_seal_key(meta_doc: &serde_json::Value, peer: &Nickname) -> Option<PublicKey> {
    let extensions = meta_doc
        .pointer(&format!("/peers/{peer}/card/capabilities/extensions"))?
        .as_array()?;
    let b58 = extensions
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
    let address = endpoint.addr();
    serde_json::json!({
        "id": address.id.to_string(),
        "relay": address.relay_urls().next().map(ToString::to_string),
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
    habilis_network::net::endpoint_addr_from_json(hint).ok()
}

/// What a frame can still grow by after it was measured: the dependency list and the sequence
/// number of a later change are not in the merge that was measured.
const FRAME_MARGIN: usize = 128;

/// The merges that publish `card` (the card as JSON, endpoint hint included) at
/// `/peers/<nick>/card`, each in a frame within [`MAX_MESSAGE_SIZE`]. `frame_len` measures the
/// frame that a merge becomes on the wire, with the sealing of a passworded gossip, so that the
/// split follows the real size and not an estimate of it. An RFC 7386 merge sets the keys of the
/// card one by one, so the card in several merges is the same card as the card in one.
///
/// The capabilities go first: they carry the seal key, and a peer can send to us as soon as it
/// has them. The other keys follow in key order. Nothing but the card is written: a reader tells
/// a whole card from a partial one by the schema of the card, see [`complete_card`].
///
/// # Errors
/// `card` is not a JSON object, a key of it does not fit a frame by itself, or `frame_len` failed.
pub(crate) fn publish_merges(
    nickname: &Nickname,
    card: &serde_json::Value,
    frame_len: impl Fn(&serde_json::Value) -> anyhow::Result<usize>,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let serde_json::Value::Object(fields) = card else {
        anyhow::bail!("the agent card is not a JSON object");
    };
    let merge_of = |part: &serde_json::Map<String, serde_json::Value>| serde_json::json!({ "peers": { nickname.as_str(): { "card": part } } });
    let fits = |merge: &serde_json::Value| -> anyhow::Result<bool> {
        Ok(frame_len(merge)? + FRAME_MARGIN <= MAX_MESSAGE_SIZE)
    };

    let mut keys: Vec<&String> = fields.keys().collect();
    keys.sort_by_key(|key| key.as_str() != "capabilities");

    let mut merges = Vec::new();
    let mut part = serde_json::Map::new();
    for key in keys {
        let mut with_key = part.clone();
        with_key.insert(key.clone(), fields[key].clone());
        if fits(&merge_of(&with_key))? {
            part = with_key;
            continue;
        }
        if part.is_empty() {
            anyhow::bail!("the card field '{key}' does not fit in one frame by itself");
        }
        merges.push(merge_of(&part));
        part = serde_json::Map::new();
        part.insert(key.clone(), fields[key].clone());
        if !fits(&merge_of(&part))? {
            anyhow::bail!("the card field '{key}' does not fit in one frame by itself");
        }
    }
    if !part.is_empty() {
        merges.push(merge_of(&part));
    }
    Ok(merges)
}

/// The card of `peer` in the meta document, when it is whole. A card that is still arriving, in
/// pieces, or whose later merge failed, lacks fields that an [`AgentCard`] requires, and is not
/// returned. The schema is the marker: the daemon writes nothing next to the card for this.
#[must_use]
pub(crate) fn complete_card<'doc>(
    meta_doc: &'doc serde_json::Value,
    peer: &Nickname,
) -> Option<&'doc serde_json::Value> {
    let card = meta_doc.pointer(&format!("/peers/{peer}/card"))?;
    serde_json::from_value::<AgentCard>(card.clone())
        .is_ok()
        .then_some(card)
}

/// The RFC 7386 merge that tells the other members why our card is not published: a member that
/// cannot seal to us then names the cause, not "cards still propagating". `None` clears it.
#[must_use]
pub(crate) fn card_error_merge(nickname: &Nickname, error: Option<&str>) -> serde_json::Value {
    serde_json::json!({ "peers": { nickname.as_str(): { "card_error": error } } })
}

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
/// `self_nick` is excluded because the roster lists peers, not self: without
/// it every node would report its own card as absent.
#[must_use]
pub(crate) fn absent_peers<'doc>(
    document: &'doc serde_json::Value,
    live: &HashSet<&str>,
    self_nick: &Nickname,
) -> Vec<&'doc str> {
    document
        .get("peers")
        .and_then(serde_json::Value::as_object)
        .map(|peers| {
            peers
                .keys()
                .map(String::as_str)
                .filter(|nick| *nick != self_nick.as_str() && !live.contains(nick))
                .collect()
        })
        .unwrap_or_default()
}

/// The RFC 7386 merge that retracts this member's whole `/peers/<nick>` meta
/// entry — card, endpoint hint, and self-reported agent facts — on graceful
/// leave. Only the entry's owner can author it: the card-forgery gate rejects
/// the same delete from anyone else.
#[must_use]
pub(crate) fn retract_merge(nickname: &Nickname) -> serde_json::Value {
    serde_json::json!({ "peers": { nickname.as_str(): null } })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use habilis_network::protocol::{AuthorKey, Nickname};

    use super::{own_card, publish_merges};

    #[test]
    fn card_declares_the_mesh_extensions_and_identity() {
        let seal_b58 = bs58::encode([7u8; 32]).into_string();
        let card = own_card(
            &Nickname::from("calm-otter"),
            AuthorKey::from_bytes([0xab; 32]),
            &seal_b58,
        );
        assert_eq!(card.name, "calm-otter");
        assert_eq!(card.capabilities.streaming, Some(true));
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
            .expect("mesh-seal extension present");
        assert_eq!(seal_ext.params.as_ref().unwrap()["x25519"], seal_b58);
        // The mesh card's only interface is the gossip binding, whose url
        // carries the cryptographic identity (no HTTP url).
        assert_eq!(card.supported_interfaces.len(), 1);
        assert_eq!(card.supported_interfaces[0].url, "ab".repeat(32));
    }

    #[test]
    fn publish_merge_targets_the_peer_card_path() {
        let nickname = Nickname::from("calm-otter");
        let seal_b58 = bs58::encode([7u8; 32]).into_string();
        let card = own_card(&nickname, AuthorKey::from_bytes([0xab; 32]), &seal_b58);
        let card = serde_json::to_value(card).unwrap();
        // A frame that is never full: the card is one merge.
        let merges = publish_merges(&nickname, &card, |_| Ok(0)).unwrap();
        assert_eq!(merges.len(), 1);
        let merge = &merges[0];
        assert_eq!(merge["peers"]["calm-otter"]["card"]["name"], "calm-otter");
        assert!(
            merge["peers"]["calm-otter"].get("model").is_none(),
            "agent-side facts are the agent's own merge, not the daemon's"
        );
    }

    /// A peer that died without a graceful leave keeps its meta entry forever:
    /// the forgery gate means no survivor can retract it. Reading the document
    /// alone therefore shows a member that is gone, still carrying whatever
    /// status it last set, which is what a peer-picking agent goes on.
    #[test]
    fn absent_peers_names_entries_with_no_live_member() {
        let document = serde_json::json!({
            "peers": {
                "calm-otter": { "status": "idle" },
                "wire-thistle": { "status": "idle" },
                "print-onyx": { "status": "busy" },
            }
        });
        let self_nick = Nickname::from("print-onyx");
        let live = HashSet::from(["calm-otter"]);

        let absent = super::absent_peers(&document, &live, &self_nick);

        assert_eq!(
            absent,
            vec!["wire-thistle"],
            "only the entry with no live peer behind it"
        );
    }

    /// The roster lists peers, never self, so our own entry has to be excluded
    /// explicitly. Without that, every node reports its own card as a ghost.
    #[test]
    fn absent_peers_never_includes_our_own_entry() {
        let document = serde_json::json!({ "peers": { "print-onyx": { "status": "idle" } } });
        let live = HashSet::new();
        assert!(super::absent_peers(&document, &live, &Nickname::from("print-onyx")).is_empty());
    }

    /// A document with no `/peers` map at all (a fresh mesh) is not an error.
    #[test]
    fn absent_peers_tolerates_a_document_without_peers() {
        let live = HashSet::new();
        assert!(
            super::absent_peers(&serde_json::json!({}), &live, &Nickname::from("print-onyx"))
                .is_empty()
        );
    }

    /// The frame that `merge` becomes on a passworded gossip: the automerge change, the `change`
    /// envelope, the seal under the channel key, the signed channel event.
    fn passworded_frame_len(merge: &serde_json::Value) -> anyhow::Result<usize> {
        use habilis_network::ops::doc::{MeshDoc, change_body, encrypt_body};
        use habilis_network::protocol::{Channel, Identity, MeshId, Message, SymmetricKey};

        let doc = MeshDoc::new_ungated();
        let Some(change) = doc.build_change(merge, &[9u8; 32])? else {
            return Ok(0);
        };
        let sealed = encrypt_body(
            &change_body(&change, None)?,
            &SymmetricKey::from_bytes([3u8; 32]),
        )?;
        let frame = Message::new_channel_event(
            &MeshId::from("test"),
            &Nickname::from("calm-otter"),
            sealed,
            Channel::Meta,
        )
        .sign(&Identity::generate());
        Ok(frame.wire_len())
    }

    /// The card of the longest nickname and a relay URL of the real kind.
    fn worst_case_card(nickname: &Nickname) -> serde_json::Value {
        let seal_b58 = bs58::encode([7u8; 32]).into_string();
        let mut card = serde_json::to_value(own_card(
            nickname,
            AuthorKey::from_bytes([0xab; 32]),
            &seal_b58,
        ))
        .unwrap();
        card["endpoint"] = serde_json::json!({
            "id": "ab".repeat(32),
            "relay": "https://euw1-1.relay.n0.iroh-canary.iroh.link./",
        });
        card
    }

    /// The bug of issue 41, measured: the card as one merge is a frame over the limit on a
    /// passworded gossip. Cut by its real size, each piece fits, with the margin, and the pieces
    /// together are the whole card.
    #[test]
    fn publish_merges_cut_the_card_by_the_measured_frame_and_make_the_same_card() {
        let nickname = Nickname::from("abcdefghij-abcdefghij-abcdefghij");
        let card = worst_case_card(&nickname);
        let whole = serde_json::json!({ "peers": { nickname.as_str(): { "card": card } } });
        let whole_len = passworded_frame_len(&whole).unwrap();
        assert!(
            whole_len > habilis_network::util::consts::MAX_MESSAGE_SIZE,
            "the premise of issue 41: the card as one merge is {whole_len} bytes on the wire"
        );

        let merges = publish_merges(&nickname, &card, passworded_frame_len)
            .expect("every field fits a frame");

        assert!(merges.len() > 1, "a card over the frame is in pieces");
        for merge in &merges {
            let len = passworded_frame_len(merge).unwrap();
            assert!(
                len + super::FRAME_MARGIN <= habilis_network::util::consts::MAX_MESSAGE_SIZE,
                "a frame of {len} bytes leaves no margin"
            );
        }
        let first = &merges[0]["peers"][nickname.as_str()]["card"];
        assert!(
            first.get("capabilities").is_some(),
            "the capabilities, which carry the seal key, are published first"
        );
        let mut rebuilt = serde_json::Map::new();
        for merge in &merges {
            let part = merge["peers"][nickname.as_str()]["card"]
                .as_object()
                .unwrap();
            for (key, value) in part {
                assert!(
                    rebuilt.insert(key.clone(), value.clone()).is_none(),
                    "{key} twice"
                );
            }
        }
        assert_eq!(serde_json::Value::Object(rebuilt), card);
    }

    /// The pieces arrive in order. No prefix of them but the whole set is served as a card, so a
    /// reader never takes a card that is still arriving. It holds because the keys of a JSON
    /// object are sorted here and `version`, a required field, is the last of them.
    #[test]
    fn no_prefix_of_the_pieces_but_the_whole_set_is_a_complete_card() {
        let nickname = Nickname::from("abcdefghij-abcdefghij-abcdefghij");
        let card = worst_case_card(&nickname);
        let merges = publish_merges(&nickname, &card, passworded_frame_len)
            .expect("every field fits a frame");
        assert!(merges.len() > 1, "the premise: the card is in pieces");

        let mut arrived = serde_json::Map::new();
        for (index, merge) in merges.iter().enumerate() {
            let part = merge["peers"][nickname.as_str()]["card"]
                .as_object()
                .unwrap();
            arrived.extend(part.clone());
            let doc = serde_json::json!({ "peers": { nickname.as_str(): { "card": arrived } } });
            assert_eq!(
                super::complete_card(&doc, &nickname).is_some(),
                index + 1 == merges.len(),
                "after piece {} of {}",
                index + 1,
                merges.len()
            );
        }
    }

    #[test]
    fn publish_merges_refuse_a_field_that_does_not_fit_a_frame_by_itself() {
        let nickname = Nickname::from("calm-otter");
        let card = serde_json::json!({ "description": "x".repeat(4000) });
        let error = publish_merges(&nickname, &card, passworded_frame_len)
            .expect_err("one field over the frame");
        assert!(error.to_string().contains("description"), "got: {error}");
    }

    /// A reader that needs the whole card must not get a card whose later merges are missing.
    #[test]
    fn complete_card_serves_a_card_only_when_the_schema_is_whole() {
        let nickname = Nickname::from("calm-otter");
        let seal_b58 = bs58::encode([7u8; 32]).into_string();
        let whole_card = serde_json::to_value(own_card(
            &nickname,
            AuthorKey::from_bytes([0xab; 32]),
            &seal_b58,
        ))
        .unwrap();
        let whole = serde_json::json!({ "peers": { "calm-otter": { "card": whole_card } } });
        assert!(super::complete_card(&whole, &nickname).is_some());

        let only_capabilities = serde_json::json!({ "peers": { "calm-otter": { "card": {
            "capabilities": whole_card["capabilities"],
        } } } });
        assert!(
            super::complete_card(&only_capabilities, &nickname).is_none(),
            "the first piece alone is not served as a card"
        );

        let none = serde_json::json!({ "peers": {} });
        assert!(super::complete_card(&none, &nickname).is_none());
    }

    #[test]
    fn card_error_merge_sets_and_clears_the_reason() {
        let nickname = Nickname::from("calm-otter");
        assert_eq!(
            super::card_error_merge(&nickname, Some("too large")),
            serde_json::json!({ "peers": { "calm-otter": { "card_error": "too large" } } })
        );
        assert_eq!(
            super::card_error_merge(&nickname, None),
            serde_json::json!({ "peers": { "calm-otter": { "card_error": null } } }),
            "an RFC 7386 null removes the key"
        );
    }

    #[test]
    fn retract_merge_nulls_the_whole_peer_entry() {
        let merge = super::retract_merge(&Nickname::from("calm-otter"));
        assert_eq!(
            merge,
            serde_json::json!({ "peers": { "calm-otter": null } }),
            "an RFC 7386 null must delete the entry itself, not a subtree"
        );
    }
}
