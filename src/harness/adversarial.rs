//! Not public API. Exposed only under the `adversarial` feature so the
//! adversarial integration suite (`tests/adversarial.rs`) can **craft and
//! inject** wire messages a correct client would never produce — unsigned /
//! bad-signature, equivocating (two messages at one `seq`), tampered, or
//! backdated — to prove receivers reject or flag them. Pairs with
//! [`crate::embed::SwarmSession::inject_raw`]. Never compiled into a
//! normal/release build, so the curated public surface is unchanged.
#![allow(missing_docs, reason = "internal test shim, doc-hidden")]
#![allow(
    missing_debug_implementations,
    reason = "opaque test-only types (wrap a non-Debug signing key); never formatted"
)]

use crate::protocol::identity::{self, Identity};
use crate::protocol::message::Message;
use crate::protocol::{MessageBody, MessageKind, Nickname, SwarmId};

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
    identity::encode_pubkey(&key.0.public())
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
    /// An open chat frame from `author` on `swarm` carrying `body` verbatim
    /// (NOT wrapped as an A2A payload — the attacker controls the raw body;
    /// use a serialized payload for a frame meant to pass the boundary). Unsigned
    /// until [`sign`](CraftedMsg::sign).
    pub fn new(swarm: &SwarmId, author: &str, body: &str) -> Self {
        let author = Nickname::new(author.to_owned()).expect("test author is a valid nickname");
        let body = MessageBody::new(body.to_owned()).expect("test body is valid");
        Self {
            msg: Message::new_a2a_msg(swarm, &author, body),
        }
    }

    /// A crafted shared-state merge (`MessageKind::State`) from `author`, whose
    /// body is the `{"k":"merge","merge":<merge>}` envelope the reducer parses.
    /// `merge` is taken verbatim, so a test can inject a non-object merge that a
    /// correct client's boundary validation would have rejected — and assert the
    /// receiver folds it as a deterministic no-op (never a panic, never a root
    /// replace). Unsigned until [`sign`](CraftedMsg::sign).
    pub fn state_merge(swarm: &SwarmId, author: &str, merge: serde_json::Value) -> Self {
        let author = Nickname::new(author.to_owned()).expect("test author is a valid nickname");
        let body = crate::daemon::state_doc::merge_body(merge)
            .expect("state merge envelope composes from any merge value");
        Self {
            msg: Message::new_state(swarm, &author, body),
        }
    }

    /// A crafted `a2a_status` frame whose FRAME correlation field carries
    /// `frame_task_id` while the PAYLOAD claims `payload_task_id` — the
    /// cross-validation attack shape a correct client never produces. Both
    /// must be valid UUIDs. Unsigned until [`sign`](CraftedMsg::sign).
    pub fn status_frame(
        swarm: &SwarmId,
        author: &str,
        to: &str,
        frame_task_id: &str,
        payload_task_id: &str,
    ) -> Self {
        let author = Nickname::new(author.to_owned()).expect("test author is a valid nickname");
        let to = Nickname::new(to.to_owned()).expect("valid target");
        let frame_tid =
            crate::a2a::TaskId::from_uuid_str(frame_task_id).expect("valid frame task id");
        let payload_tid =
            crate::a2a::TaskId::from_uuid_str(payload_task_id).expect("valid payload task id");
        let update = crate::a2a::gossip::status_update(
            swarm,
            &payload_tid,
            crate::a2a::TaskState::Working,
            None,
            None,
        );
        let body = crate::a2a::gossip::payload_body(&update).expect("crafted payload serializes");
        Self {
            msg: Message::new_frame(
                swarm,
                &author,
                MessageKind::A2aStatus {
                    to,
                    task_id: frame_tid,
                },
                body,
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
    /// consistent with the frame's current id / swarm, so the frame passes the
    /// receiver's A2A boundary gate. Call after any `id` mutation and before
    /// `sign` — the attack under test is then something *other* than a
    /// malformed payload. A frame built without this carries its raw body
    /// verbatim and is dropped at the boundary.
    pub fn wrap_a2a(mut self) -> Self {
        assert!(
            matches!(&self.msg.kind, MessageKind::A2aMsg),
            "wrap_a2a applies to chat frames only"
        );
        let mut payload = crate::a2a::gossip::chat_message(&self.msg.swarm, self.msg.body.as_str());
        payload.message_id = crate::a2a::MessageId::from_uuid_str(self.msg.id.as_str())
            .expect("frame id is a valid uuid");
        self.msg.body =
            crate::a2a::gossip::payload_body(&payload).expect("crafted payload serializes");
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

    /// Flip the kind between `Msg` and `Notice` (keeping `reply`). After
    /// signing, this is the kind-demotion attack: a relay rewriting a notice
    /// into an auto-replyable msg (or vice versa) — the signature must break.
    pub fn flip_chat_kind(mut self) -> Self {
        self.msg.kind = match self.msg.kind.clone() {
            MessageKind::Msg { reply } => MessageKind::Notice { reply },
            MessageKind::Notice { reply } => MessageKind::Msg { reply },
            MessageKind::Presence { .. }
            | MessageKind::PeerInfo
            | MessageKind::Digest
            | MessageKind::StateDigest
            | MessageKind::MetaDigest
            | MessageKind::Ping
            | MessageKind::Pong { .. }
            | MessageKind::Task { .. }
            | MessageKind::State
            | MessageKind::Meta => panic!("flip_chat_kind takes a chat message"),
        };
        self
    }

    /// This message's content hash — use as another message's `prev`/parent.
    #[must_use]
    pub fn content_hash_hex(&self) -> String {
        self.msg.content_hash_hex()
    }

    /// The serialized wire bytes to hand to `SwarmSession::inject_raw`.
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        self.msg.serialize().expect("crafted message serializes")
    }
}
