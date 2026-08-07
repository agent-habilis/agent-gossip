//! Not public API. Exposed only under the `adversarial` feature so the
//! adversarial integration suite (`tests/adversarial.rs`) can **craft and
//! inject** wire messages a correct client would never produce — unsigned /
//! bad-signature, equivocating (two messages at one `seq`), tampered, or
//! backdated — to prove receivers reject or flag them. Pairs with
//! [`crate::api::MeshSession::inject_raw`]. Never compiled into a
//! normal/release build, so the curated public surface is unchanged.
#![allow(missing_docs, reason = "internal test shim, doc-hidden")]
#![allow(
    missing_debug_implementations,
    reason = "opaque test-only types (wrap a non-Debug signing key); never formatted"
)]

use crate::a2a::wire;
use fofoca::protocol::Message;
use fofoca::protocol::PresenceSubtype;
use fofoca::protocol::{
    AppFrameParams, AppTag, CorrId, MeshId, MessageBody, MessageKind, Nickname,
};
use fofoca::protocol::{Identity, encode_pubkey};

// The reassembly byte budgets, so the suite's tripwires assert against the
// same constants the store enforces.
pub use fofoca::util::consts::{
    REASSEMBLY_AUTHOR_BUDGET_BYTES, REASSEMBLY_GROUP_MAX_BYTES, REASSEMBLY_TOTAL_BUDGET_BYTES,
};

/// An opaque attacker/peer signing key. Wraps the crate-internal `Identity`
/// so a test can hold one and pass it to the builder/helpers without the
/// rest of the `pub(crate)` identity surface leaking.
#[must_use]
pub struct TestKey(Identity);

/// Mint a fresh signing key — an attacker / peer identity.
pub fn new_key() -> TestKey {
    TestKey(Identity::generate())
}

/// The lowercase-hex public key of `key` — the value that appears as the
/// `pubkey` in the JSON event.
#[must_use]
pub fn pubkey_hex(key: &TestKey) -> String {
    encode_pubkey(&key.0.public())
}

/// The value cluster for [`CraftedMsg::status_frame`]: the author/target
/// nicknames plus the frame-vs-payload task-id pair under test.
#[derive(Clone, Copy)]
pub struct StatusFrameParams<'a> {
    pub author: &'a str,
    pub to: &'a str,
    pub frame_task_id: &'a str,
    pub payload_task_id: &'a str,
}

/// The value cluster for [`CraftedMsg::sealed_task_status`]: a *well-formed*
/// status leg for an existing task, from an author who is not that task's
/// counterparty.
#[derive(Clone, Copy)]
pub struct SealedTaskStatusParams<'a> {
    pub author: &'a str,
    pub to: &'a str,
    pub task_id: &'a str,
    pub state: crate::a2a::TaskState,
    /// Marks the leg as a liveness beat rather than a transition.
    pub beat: bool,
    /// The addressee's public mesh-seal key, as any member can read it from
    /// the meta document — see [`peer_seal_key`].
    pub seal_key: [u8; 32],
}

/// The addressee's public X25519 mesh-seal key, read out of a meta document
/// exactly as the daemon's own send path reads it. Any mesh member holds this
/// document, which is the point: sealing to a peer needs no secret.
#[must_use]
pub fn peer_seal_key(meta_doc: &serde_json::Value, peer: &str) -> Option<[u8; 32]> {
    let peer = Nickname::new(peer.to_owned()).ok()?;
    crate::a2a::card::peer_seal_key(meta_doc, &peer)
}

/// Builder for a crafted `Msg`. Choose every field, then `sign` (or not) and
/// take the wire [`bytes`](CraftedMsg::bytes). Mutating *after* `sign` (e.g.
/// [`tamper_body`](CraftedMsg::tamper_body)) yields a structurally-valid
/// message whose signature no longer matches — the tampering case.
#[must_use]
pub struct CraftedMsg {
    msg: Message,
}

impl CraftedMsg {
    /// An open chat frame from `author` on `mesh` carrying `body` verbatim
    /// (NOT wrapped as an A2A payload — the attacker controls the raw body;
    /// use a serialized payload for a frame meant to pass the boundary). Unsigned
    /// until [`sign`](CraftedMsg::sign).
    pub fn new(mesh: &MeshId, author: &str, body: &str) -> Self {
        let author = Nickname::new(author.to_owned()).expect("test author is a valid nickname");
        let body = MessageBody::new(body.to_owned()).expect("test body is valid");
        Self {
            msg: Message::new_app(
                mesh,
                &author,
                AppFrameParams {
                    tag: AppTag::from(wire::BROADCAST),
                    to: None,
                    corr: None,
                    body,
                },
            ),
        }
    }

    /// Re-address this chat frame, producing one of the two shapes a correct
    /// client cannot emit — the pair that together form the chat privacy
    /// boundary:
    ///
    /// - `Some(nick)` on a `a2a_broadcast` frame: a private line dressed as
    ///   gossip-wide.
    /// - `None` on an `a2a_msg` frame: a msg handed to every peer.
    ///
    /// The payload is left as-is, so what is under test is the addressing
    /// rather than a malformed body. Call before `sign` — `to` is part of the
    /// canonical bytes.
    pub fn address_to(mut self, to: Option<&str>) -> Self {
        let MessageKind::App { tag, corr, .. } = self.msg.kind.clone() else {
            panic!("address_to takes an app frame");
        };
        self.msg.kind = MessageKind::App {
            tag,
            to: to.map(|nick| {
                Nickname::new(nick.to_owned()).expect("test addressee is a valid nickname")
            }),
            corr,
        };
        self
    }

    /// Retag this chat frame as a `a2a_msg` (chat addressed to one peer),
    /// keeping the body. Pair with [`address_to`](CraftedMsg::address_to) to
    /// build either a well-formed msg or the broadcast-shaped forgery.
    pub fn as_msg(mut self) -> Self {
        let MessageKind::App { to, corr, .. } = self.msg.kind.clone() else {
            panic!("as_msg takes an app frame");
        };
        self.msg.kind = MessageKind::App {
            tag: AppTag::from(wire::MSG),
            to,
            corr,
        };
        self
    }

    /// Wrap the body as a valid **msg** A2A payload (no broadcast extension),
    /// the directed counterpart of [`wrap_a2a`](CraftedMsg::wrap_a2a). Call
    /// after any `id` mutation and before `sign`.
    pub fn wrap_msg(mut self) -> Self {
        assert!(
            self.msg.kind.is_app(wire::MSG),
            "wrap_msg applies to msg frames only"
        );
        let mut payload = crate::a2a::gossip::compose_msg(&self.msg.mesh, self.msg.body.as_str());
        payload.message_id = crate::a2a::MessageId::from_uuid_str(self.msg.id.as_str())
            .expect("frame id is a valid uuid");
        self.msg.body =
            crate::a2a::gossip::payload_body(&payload).expect("crafted payload serializes");
        self
    }

    /// A crafted shared-state merge (`MessageKind::State`) from `author`, whose
    /// body is the legacy `{"k":"merge","merge":<merge>}` envelope. The automerge
    /// engine no longer folds this shape (it is a deterministic no-op), so a test
    /// can assert the receiver never applies it — never a panic, never a change.
    /// Unsigned until [`sign`](CraftedMsg::sign).
    pub fn state_merge(mesh: &MeshId, author: &str, merge: serde_json::Value) -> Self {
        let author = Nickname::new(author.to_owned()).expect("test author is a valid nickname");
        let body = fofoca::ops::doc::merge_body(merge)
            .expect("state merge envelope composes from any merge value");
        Self {
            msg: Message::new_state(mesh, &author, body),
        }
    }

    /// A crafted `meta` frame carrying a real automerge change that forges
    /// `victim`'s `/peers/<victim>/card` — the receive-side card-forgery a
    /// correct client's own gate refuses to author. Built on the deterministic
    /// `/peers` genesis, so its only causal dep is one every meta replica holds:
    /// the change is thus *applicable* at the victim and actually exercises the
    /// forgery gate (rather than being harmlessly buffered for missing deps).
    /// Unsigned until [`sign`](CraftedMsg::sign).
    pub fn meta_forge_card(
        mesh: &MeshId,
        author: &str,
        victim: &str,
        fake_card: &serde_json::Value,
    ) -> Self {
        let author = Nickname::new(author.to_owned()).expect("test author is a valid nickname");
        let merge = serde_json::json!({ "peers": { victim: { "card": fake_card } } });
        // `build_change` composes the change without the gate (the gate lives in
        // `ingest`), exactly as a malicious peer bypassing our write path would.
        // The attacker picks its own actor seed; the nickname bytes serve.
        // Gated, so the forge is built on a replica that shares the victim's
        // genesis — hence the same `peers` object id. Built on an *ungated* doc it
        // would be a different map, and whether the victim's gate even fires
        // would come down to which of two conflicting maps automerge keeps.
        let change = fofoca::ops::doc::MeshDoc::new_gated(crate::a2a::card_gate())
            .build_change(&merge, author.as_str().as_bytes())
            .expect("forge change builds")
            .expect("forge merge is not a no-op");
        let body = fofoca::ops::doc::change_body(&change, None).expect("change body composes");
        Self {
            msg: Message::new_channel_event(mesh, &author, body, fofoca::protocol::Channel::Meta),
        }
    }

    /// A crafted `a2a_status` frame whose FRAME correlation field (`corr`)
    /// carries `frame_task_id` while the PAYLOAD claims `payload_task_id` — the
    /// cross-validation attack shape a correct client never produces. Both
    /// must be valid UUIDs. Unsigned until [`sign`](CraftedMsg::sign).
    pub fn status_frame(mesh: &MeshId, params: StatusFrameParams<'_>) -> Self {
        let StatusFrameParams {
            author,
            to,
            frame_task_id,
            payload_task_id,
        } = params;
        let author = Nickname::new(author.to_owned()).expect("test author is a valid nickname");
        let to = Nickname::new(to.to_owned()).expect("valid target");
        let frame_tid =
            crate::a2a::TaskId::from_uuid_str(frame_task_id).expect("valid frame task id");
        let payload_tid =
            crate::a2a::TaskId::from_uuid_str(payload_task_id).expect("valid payload task id");
        let update = crate::a2a::gossip::status_update(
            mesh,
            crate::a2a::gossip::StatusUpdateParams {
                task_id: &payload_tid,
                state: crate::a2a::TaskState::Working,
                note: None,
                metadata: None,
            },
        );
        let body = crate::a2a::gossip::payload_body(&update).expect("crafted payload serializes");
        Self {
            msg: Message::new_app(
                mesh,
                &author,
                AppFrameParams {
                    tag: AppTag::from(wire::STATUS),
                    to: Some(to),
                    corr: Some(CorrId::from(frame_tid.as_str())),
                    body,
                },
            ),
        }
    }

    /// A directed `a2a_status` leg that is correct in every respect the
    /// boundary checks — right tag, right addressing, payload `taskId` and
    /// `contextId` consistent, body sealed to the addressee — and wrong only in
    /// who authored it. The shape a bystander produces to drive a task it is
    /// not a party to; sealing needs nothing secret, since the addressee's key
    /// is published in the meta document.
    pub fn sealed_task_status(mesh: &MeshId, params: SealedTaskStatusParams<'_>) -> Self {
        let SealedTaskStatusParams {
            author,
            to,
            task_id,
            state,
            beat,
            seal_key,
        } = params;
        let author = Nickname::new(author.to_owned()).expect("test author is a valid nickname");
        let to = Nickname::new(to.to_owned()).expect("valid target");
        let task_id = crate::a2a::TaskId::from_uuid_str(task_id).expect("valid task id");
        let update = crate::a2a::gossip::status_update(
            mesh,
            crate::a2a::gossip::StatusUpdateParams {
                task_id: &task_id,
                state,
                note: None,
                metadata: beat.then(|| serde_json::json!({ crate::a2a::META_BEAT: true })),
            },
        );
        let body = crate::a2a::gossip::payload_body(&update).expect("crafted payload serializes");
        let body = fofoca::protocol::seal_to_body(&seal_key, body.as_str())
            .expect("sealing to a published key succeeds");
        Self {
            msg: Message::new_app(
                mesh,
                &author,
                AppFrameParams {
                    tag: AppTag::from(wire::STATUS),
                    to: Some(to),
                    corr: None,
                    body,
                },
            ),
        }
    }

    /// Stamp the per-author hash chain (Phase 2): `seq` + optional `prev`.
    pub fn chain(mut self, seq: u64, prev: Option<String>) -> Self {
        self.msg = self.msg.with_chain(seq, prev);
        self
    }

    /// Override the message id — e.g. to replay another message's id (the
    /// dedup-ordering test). Must be a valid UUID string.
    pub fn id(mut self, id: &str) -> Self {
        self.msg.id = id.parse().expect("crafted id must be a valid UUID");
        self
    }

    /// Wrap the current raw body into a **valid** A2A broadcast chat payload
    /// consistent with the frame's current id / mesh, so the frame passes the
    /// receiver's A2A boundary gate. Call after any `id` mutation and before
    /// `sign` — the attack under test is then something *other* than a
    /// malformed payload. A frame built without this carries its raw body
    /// verbatim and is dropped at the boundary.
    pub fn wrap_a2a(mut self) -> Self {
        assert!(
            self.msg.kind.is_app(wire::BROADCAST),
            "wrap_a2a applies to chat frames only"
        );
        let mut payload =
            crate::a2a::gossip::compose_broadcast(&self.msg.mesh, self.msg.body.as_str());
        payload.message_id = crate::a2a::MessageId::from_uuid_str(self.msg.id.as_str())
            .expect("frame id is a valid uuid");
        self.msg.body =
            crate::a2a::gossip::payload_body(&payload).expect("crafted payload serializes");
        self
    }

    /// Tag the frame as one shard of a multipart group — the crafted headers
    /// the reassembly-budget tripwires inject (never-completing groups,
    /// out-of-range indexes). `group` must be a valid UUID string; `idx`/
    /// `total` are taken verbatim, valid or not (the receiver's parse gate is
    /// the thing under test).
    pub fn shard(mut self, group: &str, idx: u32, total: u32) -> Self {
        let group =
            fofoca::protocol::ShardGroup::from_uuid_str(group).expect("valid shard group uuid");
        self.msg = self
            .msg
            .with_shard(Some(fofoca::protocol::Shard { group, idx, total }));
        self
    }

    /// Stamp the cross-author DAG `parents` (Phase 3).
    pub fn parents(mut self, parents: Vec<String>) -> Self {
        self.msg = self.msg.with_parents(parents);
        self
    }

    /// Set the timestamp (e.g. an implausibly old or future value).
    pub fn timestamp(mut self, ts: i64) -> Self {
        self.msg.timestamp = ts;
        self
    }

    /// Sign with `key`, binding the current fields. Call last — unless the
    /// test intends to tamper afterward.
    pub fn sign(mut self, key: &TestKey) -> Self {
        self.msg = self.msg.signed(&key.0);
        self
    }

    /// Mutate the body *after* signing → a tampered message whose signature
    /// no longer matches its canonical bytes.
    pub fn tamper_body(mut self, body: &str) -> Self {
        self.msg.body = MessageBody::new(body.to_owned()).expect("test body is valid");
        self
    }

    /// Rewrite the kind after signing — the kind-tamper attack. The kind is
    /// part of the canonical bytes, so a relay flipping a signed broadcast chat
    /// `A2aMsg` into a `Presence` beat (or the reverse) must break the
    /// signature and the victim must drop it.
    pub fn flip_chat_kind(mut self) -> Self {
        self.msg.kind = match self.msg.kind.clone() {
            MessageKind::App { tag, .. } if tag.as_str() == wire::BROADCAST => {
                MessageKind::Presence {
                    subtype: PresenceSubtype::Alive,
                }
            }
            MessageKind::Presence { .. } => MessageKind::App {
                tag: AppTag::from(wire::BROADCAST),
                to: None,
                corr: None,
            },
            MessageKind::App { .. }
            | MessageKind::PeerInfo
            | MessageKind::Digest
            | MessageKind::StateDigest
            | MessageKind::MetaDigest
            | MessageKind::Ping
            | MessageKind::Pong { .. }
            | MessageKind::State
            | MessageKind::Meta
            | MessageKind::LinkState => panic!("flip_chat_kind takes a broadcast chat message"),
        };
        self
    }

    /// This message's content hash — use as another message's `prev`/parent.
    #[must_use]
    pub fn content_hash_hex(&self) -> String {
        self.msg.content_hash_hex()
    }

    /// The serialized wire bytes to hand to `MeshSession::inject_raw`.
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        self.msg.serialize().expect("crafted message serializes")
    }
}
