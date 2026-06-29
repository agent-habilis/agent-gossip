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
    /// An open `Msg` from `author` on `swarm` carrying `body`. Unsigned
    /// until [`sign`](CraftedMsg::sign).
    pub fn new(swarm: &SwarmId, author: &str, body: &str) -> Self {
        let author = Nickname::new(author.to_owned()).expect("test author is a valid nickname");
        let body = MessageBody::new(body.to_owned()).expect("test body is valid");
        Self {
            msg: Message::new_message(swarm, &author, body),
        }
    }

    /// A crafted shared-state patch (`MessageKind::State`) from `author`, whose
    /// body is the `{"k":"patch","ops":<ops>}` envelope the reducer parses.
    /// `ops` is taken verbatim, so a test can inject an out-of-subset /
    /// non-applying / malformed op array that a correct client's boundary
    /// validation would have rejected — and assert the receiver folds it as a
    /// deterministic no-op (never a panic, never a partial apply). Unsigned
    /// until [`sign`](CraftedMsg::sign).
    pub fn state_patch(swarm: &SwarmId, author: &str, ops: serde_json::Value) -> Self {
        let author = Nickname::new(author.to_owned()).expect("test author is a valid nickname");
        let body = crate::daemon::state_doc::patch_body(ops)
            .expect("state patch envelope composes from any ops value");
        Self {
            msg: Message::new_state(swarm, &author, body),
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

    /// Make this a directed reply addressed to `target` instead of an open
    /// `Msg` — a message a receiver relays but, if `target` is someone else,
    /// never logs (so it must never be folded into the fork/DAG indexes).
    pub fn reply_to(mut self, target: &str) -> Self {
        let target = Nickname::new(target.to_owned()).expect("valid reply target");
        self.msg.kind = MessageKind::Msg {
            reply: Some(target),
        };
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
