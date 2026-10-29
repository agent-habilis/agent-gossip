//! Not public API. Exposed only under the `adversarial` feature so the
//! adversarial integration suite (`tests/adversarial.rs`) can **craft and
//! inject** wire messages a correct client would never produce — unsigned /
//! bad-signature, equivocating (two messages at one `seq`), tampered, or
//! backdated — to prove receivers reject or flag them. Pairs with
//! [`crate::api::MeshSession::inject_raw`]. Never compiled into a
//! normal/release build, so the curated public surface is unchanged.

use habilis_network::ops::doc;
use habilis_network::ops::doc::MeshDoc;
use habilis_network::protocol::{
    AppFrameParams, ContentHash, Identity, MeshId, Message, MessageBody, MessageKind, Nickname,
    PresenceSubtype, UnixSecs, encode_pubkey,
};
// The reassembly byte budgets, so the suite's tripwires assert against the
// same constants the store enforces.
pub use habilis_network::util::consts::{
    REASSEMBLY_AUTHOR_BUDGET_BYTES, REASSEMBLY_GROUP_MAX_BYTES, REASSEMBLY_TOTAL_BUDGET_BYTES,
};
use habilis_network::x25519_dalek::PublicKey;

use crate::a2a::receipt::Receipt;
use crate::a2a::receipt::ReceiptState::Read;
use crate::a2a::{card, status_update_params, wire};

/// An opaque attacker/peer signing key. Wraps the crate-internal `Identity`
/// so a test can hold one and pass it to the builder/helpers without the
/// rest of the `pub(crate)` identity surface leaking.
#[must_use]
#[derive(Debug)]
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
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct StatusFrameParams<'caller> {
    /// Nickname of the message author.
    pub author: &'caller str,
    /// Nickname of the addressee.
    pub to: &'caller str,
    /// Task id in the frame header.
    pub frame_task_id: &'caller str,
    /// Task id inside the signed payload.
    pub payload_task_id: &'caller str,
}

/// The value cluster for [`CraftedMsg::sealed_task_status`]: a *well-formed*
/// status leg for an existing task, from an author who is not that task's
/// counterparty.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct SealedTaskStatusParams<'caller> {
    /// Nickname of the message author.
    pub author: &'caller str,
    /// Nickname of the addressee.
    pub to: &'caller str,
    /// Id of the task that the leg updates.
    pub task_id: &'caller str,
    /// Task state that the leg reports.
    pub state: crate::a2a::TaskState,
    /// Marks the leg as a liveness beat rather than a transition.
    pub beat: bool,
    /// The addressee's public mesh-seal key, as any member can read it from
    /// the meta document — see [`peer_seal_key`].
    pub seal_key: PublicKey,
}

/// The value cluster for [`CraftedMsg::sealed_msg`].
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct SealedMsgParams<'caller> {
    /// Nickname of the message author.
    pub author: &'caller str,
    /// Nickname of the addressee.
    pub to: &'caller str,
    /// Message text before sealing.
    pub text: &'caller str,
    /// The addressee's public mesh-seal key.
    pub seal_key: PublicKey,
}

impl<'caller> StatusFrameParams<'caller> {
    /// Params with every field set.
    #[must_use]
    pub fn new(
        author: &'caller str,
        to: &'caller str,
        frame_task_id: &'caller str,
        payload_task_id: &'caller str,
    ) -> Self {
        Self {
            author,
            to,
            frame_task_id,
            payload_task_id,
        }
    }
}

impl<'caller> SealedTaskStatusParams<'caller> {
    /// A `working` transition, not a beat.
    #[must_use]
    pub fn new(
        author: &'caller str,
        to: &'caller str,
        task_id: &'caller str,
        seal_key: PublicKey,
    ) -> Self {
        Self {
            author,
            to,
            task_id,
            state: crate::a2a::TaskState::Working,
            beat: false,
            seal_key,
        }
    }

    /// Replaces the reported task state.
    #[must_use]
    pub fn with_state(mut self, state: crate::a2a::TaskState) -> Self {
        self.state = state;
        self
    }
}

impl<'caller> SealedMsgParams<'caller> {
    /// Params with every field set.
    #[must_use]
    pub fn new(
        author: &'caller str,
        to: &'caller str,
        text: &'caller str,
        seal_key: PublicKey,
    ) -> Self {
        Self {
            author,
            to,
            text,
            seal_key,
        }
    }
}

/// The addressee's public X25519 mesh-seal key, read out of a meta document
/// exactly as the daemon's own send path reads it. Any mesh member holds this
/// document, which is the point: sealing to a peer needs no secret.
#[must_use]
pub fn peer_seal_key(meta_doc: &serde_json::Value, peer: &str) -> Option<PublicKey> {
    let peer = Nickname::try_new(peer).ok()?;
    card::peer_seal_key(meta_doc, &peer)
}

/// Builder for a crafted `Msg`. Choose every field, then `sign` (or not) and
/// take the wire [`bytes`](CraftedMsg::bytes). Mutating *after* `sign` (e.g.
/// [`tamper_body`](CraftedMsg::tamper_body)) yields a structurally-valid
/// message whose signature no longer matches — the tampering case.
#[must_use]
#[derive(Debug)]
pub struct CraftedMsg {
    message: Message,
}

impl CraftedMsg {
    /// An open chat frame from `author` on `mesh` carrying `body` verbatim
    /// (NOT wrapped as an A2A payload — the attacker controls the raw body;
    /// use a serialized payload for a frame meant to pass the boundary). Unsigned
    /// until [`sign`](CraftedMsg::sign).
    ///
    /// # Panics
    ///
    /// Panics unless the author is a valid nickname.
    pub fn new(mesh: &MeshId, author: &str, body: &str) -> Self {
        let author = Nickname::try_new(author).expect("test author is a valid nickname");
        let body = MessageBody::try_new(body).expect("test body is valid");
        Self {
            message: Message::new_app(
                mesh,
                &author,
                AppFrameParams::new(wire::app_tag(wire::BROADCAST), None, None, body),
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
    ///
    /// # Panics
    ///
    /// Panics unless the addressee is a valid nickname.
    pub fn address_to(mut self, to: Option<&str>) -> Self {
        let MessageKind::App { tag, corr, .. } = self.message.kind else {
            panic!("address_to takes an app frame");
        };
        self.message.kind = MessageKind::App {
            tag,
            to: to.map(|nick| Nickname::try_new(nick).expect("test addressee is a valid nickname")),
            corr,
        };
        self
    }

    /// Retag this chat frame as a `a2a_msg` (chat addressed to one peer),
    /// keeping the body. Pair with [`address_to`](CraftedMsg::address_to) to
    /// build either a well-formed msg or the broadcast-shaped forgery.
    ///
    /// # Panics
    ///
    /// Panics unless the message is an app frame.
    pub fn into_msg(mut self) -> Self {
        let MessageKind::App { to, corr, .. } = self.message.kind else {
            panic!("into_msg takes an app frame");
        };
        self.message.kind = MessageKind::App {
            tag: wire::app_tag(wire::MSG),
            to,
            corr,
        };
        self
    }

    /// Wrap the body as a valid **msg** A2A payload (no broadcast extension),
    /// the directed counterpart of [`wrap_a2a`](CraftedMsg::wrap_a2a). Call
    /// after any `id` mutation and before `sign`.
    ///
    /// # Panics
    ///
    /// Panics unless the frame id is a valid UUID.
    pub fn wrap_msg(mut self) -> Self {
        assert!(
            self.message.kind.is_app(wire::MSG),
            "wrap_msg applies to msg frames only"
        );
        let mut payload =
            status_update_params::compose_msg(&self.message.mesh, self.message.body.as_str());
        payload.message_id = crate::a2a::MessageId::from_uuid_str(self.message.id.as_str())
            .expect("frame id is a valid uuid");
        self.message.body =
            status_update_params::payload_body(&payload).expect("crafted payload serializes");
        self
    }

    /// A crafted shared-state merge (`MessageKind::State`) from `author`, whose
    /// body is the legacy `{"k":"merge","merge":<merge>}` envelope. The automerge
    /// engine no longer folds this shape (it is a deterministic no-op), so a test
    /// can assert the receiver never applies it — never a panic, never a change.
    /// Unsigned until [`sign`](CraftedMsg::sign).
    ///
    /// # Panics
    ///
    /// Panics unless the author is a valid nickname.
    pub fn state_merge(mesh: &MeshId, author: &str, merge: serde_json::Value) -> Self {
        let author = Nickname::try_new(author).expect("test author is a valid nickname");
        let body =
            doc::merge_body(merge).expect("state merge envelope composes from any merge value");
        Self {
            message: Message::new_state(mesh, &author, body),
        }
    }

    /// A crafted `meta` frame carrying a real automerge change that forges
    /// `victim`'s `/peers/<victim>/card` — the receive-side card-forgery a
    /// correct client's own gate refuses to author. Built on the deterministic
    /// `/peers` genesis, so its only causal dep is one every meta replica holds:
    /// the change is thus *applicable* at the victim and actually exercises the
    /// forgery gate (rather than being harmlessly buffered for missing deps).
    /// Unsigned until [`sign`](CraftedMsg::sign).
    ///
    /// # Panics
    ///
    /// Panics unless the author is a valid nickname.
    pub fn meta_forge_card(
        mesh: &MeshId,
        author: &str,
        victim: &str,
        fake_card: &serde_json::Value,
    ) -> Self {
        let author = Nickname::try_new(author).expect("test author is a valid nickname");
        let merge = serde_json::json!({ "peers": { victim: { "card": fake_card } } });
        // `build_change` composes the change without the gate (the gate lives in
        // `ingest`), exactly as a malicious peer bypassing our write path would.
        // The attacker picks its own actor seed; the nickname bytes serve.
        // Gated, so the forge is built on a replica that shares the victim's
        // genesis — hence the same `peers` object id. Built on an *ungated* doc it
        // would be a different map, and whether the victim's gate even fires
        // would come down to which of two conflicting maps automerge keeps.
        let author_bytes: &[u8] = author.as_str().as_bytes();
        let change: Vec<u8> = MeshDoc::new_gated(crate::a2a::card_gate())
            .build_change(&merge, author_bytes)
            .expect("forge change builds")
            .expect("forge merge is not a no-op");
        let body = doc::change_body(&change, None).expect("change body composes");
        Self {
            message: Message::new_channel_event(
                mesh,
                &author,
                body,
                habilis_network::protocol::Channel::Meta,
            ),
        }
    }

    /// A crafted `a2a_status` frame whose FRAME correlation field (`corr`)
    /// carries `frame_task_id` while the PAYLOAD claims `payload_task_id` — the
    /// cross-validation attack shape a correct client never produces. Both
    /// must be valid UUIDs. Unsigned until [`sign`](CraftedMsg::sign).
    ///
    /// # Panics
    ///
    /// Panics unless the author is a valid nickname.
    pub fn status_frame(mesh: &MeshId, params: StatusFrameParams<'_>) -> Self {
        let StatusFrameParams {
            author,
            to,
            frame_task_id,
            payload_task_id,
        } = params;
        let author = Nickname::try_new(author).expect("test author is a valid nickname");
        let to = Nickname::try_new(to).expect("valid target");
        let frame_tid =
            crate::a2a::TaskId::from_uuid_str(frame_task_id).expect("valid frame task id");
        let payload_tid =
            crate::a2a::TaskId::from_uuid_str(payload_task_id).expect("valid payload task id");
        let update = status_update_params::status_update(
            mesh,
            status_update_params::StatusUpdateParams::new(
                &payload_tid,
                crate::a2a::TaskState::Working,
            ),
        );
        let body = status_update_params::payload_body(&update).expect("crafted payload serializes");
        Self {
            message: Message::new_app(
                mesh,
                &author,
                AppFrameParams::new(
                    wire::app_tag(wire::STATUS),
                    Some(to),
                    Some(wire::corr_id(frame_tid.as_str())),
                    body,
                ),
            ),
        }
    }

    /// A directed `a2a_status` leg that is correct in every respect the
    /// boundary checks — right tag, right addressing, payload `taskId` and
    /// `contextId` consistent, body sealed to the addressee — and wrong only in
    /// who authored it. The shape a bystander produces to drive a task it is
    /// not a party to; sealing needs nothing secret, since the addressee's key
    /// is published in the meta document.
    ///
    /// # Panics
    ///
    /// Panics unless the author is a valid nickname.
    pub fn sealed_task_status(mesh: &MeshId, params: SealedTaskStatusParams<'_>) -> Self {
        let SealedTaskStatusParams {
            author,
            to,
            task_id,
            state,
            beat,
            seal_key,
        } = params;
        let author = Nickname::try_new(author).expect("test author is a valid nickname");
        let to = Nickname::try_new(to).expect("valid target");
        let task_id = crate::a2a::TaskId::from_uuid_str(task_id).expect("valid task id");
        let update = status_update_params::status_update(
            mesh,
            status_update_params::StatusUpdateParams {
                task_id: &task_id,
                state,
                note: None,
                metadata: beat.then(|| serde_json::json!({ crate::a2a::META_BEAT: true })),
            },
        );
        let body = status_update_params::payload_body(&update).expect("crafted payload serializes");
        let body = habilis_network::protocol::seal_to_body(&seal_key, body.as_str())
            .expect("sealing to a published key succeeds");
        Self {
            message: Message::new_app(
                mesh,
                &author,
                AppFrameParams::new(wire::app_tag(wire::STATUS), Some(to), None, body),
            ),
        }
    }

    /// A directed `a2a_msg` with a valid payload, sealed to the addressee.
    ///
    /// # Panics
    ///
    /// Panics unless the author is a valid nickname.
    pub fn sealed_msg(mesh: &MeshId, params: SealedMsgParams<'_>) -> Self {
        let SealedMsgParams {
            author,
            to,
            text,
            seal_key,
        } = params;
        let author = Nickname::try_new(author).expect("test author is a valid nickname");
        let to = Nickname::try_new(to).expect("valid target");
        let mut frame = Message::new_app(
            mesh,
            &author,
            AppFrameParams::new(
                wire::app_tag(wire::MSG),
                Some(to),
                None,
                MessageBody::try_new("").expect("empty body is valid"),
            ),
        );
        let mut payload = status_update_params::compose_msg(mesh, text);
        payload.message_id =
            crate::a2a::MessageId::from_uuid_str(frame.id.as_str()).expect("frame id is a uuid");
        let body =
            status_update_params::payload_body(&payload).expect("crafted payload serializes");
        frame.body = habilis_network::protocol::seal_to_body(&seal_key, body.as_str())
            .expect("sealing to a published key succeeds");
        Self { message: frame }
    }

    /// A sealed `a2a_receipt` that says `to` sent frame `ref_id` and that it
    /// was read: what a bystander forges with a frame id it saw in a header.
    ///
    /// # Panics
    ///
    /// Panics unless the author is a valid nickname.
    pub fn sealed_receipt(mesh: &MeshId, params: SealedMsgParams<'_>) -> Self {
        let SealedMsgParams {
            author,
            to,
            text: ref_id,
            seal_key,
        } = params;
        let author = Nickname::try_new(author).expect("test author is a valid nickname");
        let to = Nickname::try_new(to).expect("valid target");
        let receipt = Receipt {
            ref_id: ref_id.to_owned(),
            state: Read,
        };
        let plain = serde_json::to_string(&receipt).expect("a receipt serializes");
        let body = habilis_network::protocol::seal_to_body(&seal_key, &plain)
            .expect("sealing to a published key succeeds");
        Self {
            message: Message::new_app(
                mesh,
                &author,
                AppFrameParams::new(wire::app_tag(wire::RECEIPT), Some(to), None, body),
            ),
        }
    }

    /// The id that a receipt for this frame names in its `ref`.
    #[must_use]
    pub fn frame_id(&self) -> String {
        self.message.id.as_str().to_owned()
    }

    /// Stamp the per-author hash chain (Phase 2): `seq` + optional `prev`.
    pub fn chain(mut self, seq: u64, prev: Option<ContentHash>) -> Self {
        self.message = self.message.with_chain(seq, prev);
        self
    }

    /// Override the message id — e.g. to replay another message's id (the
    /// dedup-ordering test). Must be a valid UUID string.
    ///
    /// # Panics
    ///
    /// Panics unless the crafted id is a valid UUID.
    pub fn id(mut self, id: &str) -> Self {
        self.message.id = id.parse().expect("crafted id must be a valid UUID");
        self
    }

    /// Wrap the current raw body into a **valid** A2A broadcast chat payload
    /// consistent with the frame's current id / mesh, so the frame passes the
    /// receiver's A2A boundary gate. Call after any `id` mutation and before
    /// `sign` — the attack under test is then something *other* than a
    /// malformed payload. A frame built without this carries its raw body
    /// verbatim and is dropped at the boundary.
    ///
    /// # Panics
    ///
    /// Panics unless the frame id is a valid UUID.
    pub fn wrap_a2a(mut self) -> Self {
        assert!(
            self.message.kind.is_app(wire::BROADCAST),
            "wrap_a2a applies to chat frames only"
        );
        let mut payload =
            status_update_params::compose_broadcast(&self.message.mesh, self.message.body.as_str());
        payload.message_id = crate::a2a::MessageId::from_uuid_str(self.message.id.as_str())
            .expect("frame id is a valid uuid");
        self.message.body =
            status_update_params::payload_body(&payload).expect("crafted payload serializes");
        self
    }

    /// Tag the frame as one shard of a multipart group — the crafted headers
    /// the reassembly-budget tripwires inject (never-completing groups,
    /// out-of-range indexes). `group` must be a valid UUID string; `idx`/
    /// `total` are taken verbatim, valid or not (the receiver's parse gate is
    /// the thing under test).
    ///
    /// # Panics
    ///
    /// Panics unless the shard group id is a valid UUID.
    pub fn shard(mut self, group: &str, index: u32, total: u32) -> Self {
        let group = habilis_network::protocol::ShardGroup::from_uuid_str(group)
            .expect("valid shard group uuid");
        self.message = self.message.with_shard(Some(
            habilis_network::protocol::Shard::new(group, total).with_index(index),
        ));
        self
    }

    /// Stamp the cross-author DAG `parents` (Phase 3).
    pub fn parents(mut self, parents: impl IntoIterator<Item = ContentHash>) -> Self {
        self.message = self.message.with_parents(parents);
        self
    }

    /// Set the timestamp (e.g. an implausibly old or future value).
    pub fn timestamp(mut self, ts: UnixSecs) -> Self {
        self.message.timestamp = ts;
        self
    }

    /// Sign with `key`, binding the current fields. Call last — unless the
    /// test intends to tamper afterward.
    pub fn sign(mut self, key: &TestKey) -> Self {
        self.message = self.message.sign(&key.0).into_message();
        self
    }

    /// Mutate the body *after* signing → a tampered message whose signature
    /// no longer matches its canonical bytes.
    ///
    /// # Panics
    ///
    /// Panics unless the body is a valid message body.
    pub fn tamper_body(mut self, body: &str) -> Self {
        self.message.body = MessageBody::try_new(body).expect("test body is valid");
        self
    }

    /// Rewrite the kind after signing — the kind-tamper attack. The kind is
    /// part of the canonical bytes, so a relay flipping a signed broadcast chat
    /// `A2aMsg` into a `Presence` beat (or the reverse) must break the
    /// signature and the victim must drop it.
    ///
    /// # Panics
    ///
    /// Panics unless the message is a broadcast chat message.
    pub fn flip_chat_kind(mut self) -> Self {
        let kind = &self.message.kind;
        let flipped = if matches!(kind, MessageKind::Presence { .. }) {
            MessageKind::App {
                tag: wire::app_tag(wire::BROADCAST),
                to: None,
                corr: None,
            }
        } else if matches!(kind, MessageKind::App { tag, .. } if tag.as_str() == wire::BROADCAST) {
            MessageKind::Presence {
                subtype: PresenceSubtype::Alive,
            }
        } else {
            panic!("flip_chat_kind takes a broadcast chat message")
        };
        self.message.kind = flipped;
        self
    }

    /// This message's content hash — use as another message's `prev`/parent.
    #[must_use]
    pub fn content_hash(&self) -> ContentHash {
        self.message.content_hash()
    }

    /// The serialized wire bytes to hand to `MeshSession::inject_raw`.
    ///
    /// # Panics
    ///
    /// Panics unless the crafted message serializes.
    #[must_use]
    pub fn bytes(&self) -> bytes::Bytes {
        use habilis_network::protocol::message::Verified;
        // A signed frame keeps its signature as built, or as stale after a tamper: the fixture keeps
        // the signer, and the engine's own serializer writes the bytes.
        if self.message.signer.is_some() {
            let frame = Verified::fixture_with_signature(self.message.clone());
            return bytes::Bytes::copy_from_slice(
                frame
                    .serialize()
                    .expect("crafted message serializes")
                    .as_bytes(),
            );
        }
        // An unsigned frame carries no key and no signature on the wire. The engine has no public
        // unsigned encoding, so the serialized frame has its two key fields removed here.
        let zero_key = habilis_network::protocol::AuthorKey::from_bytes([0; 32]);
        let frame = Verified::fixture_unchecked(self.message.clone(), zero_key);
        let mut wire: serde_json::Value = serde_json::from_slice(
            frame
                .serialize()
                .expect("crafted message serializes")
                .as_bytes(),
        )
        .expect("a serialized frame is JSON");
        let object = wire.as_object_mut().expect("a wire frame is a JSON object");
        object.remove("pubkey");
        object.remove("sig");
        bytes::Bytes::from(serde_json::to_vec(&wire).expect("an unsigned frame serializes"))
    }
}
