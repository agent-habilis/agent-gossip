//! The shared-document engine backing the `state` and `meta` channels.
//!
//! Each channel is an [`automerge`] CRDT. A local write is expressed as an
//! RFC 7386-style JSON merge (the unchanged `agent-gossip state|meta merge` surface),
//! translated into one automerge change; peers exchange those changes and
//! automerge merges them conflict-free — so we no longer own an ordered-log
//! fold. Convergence is automerge's job; ours is authenticity.
//!
//! Every change is still carried inside a signed [`Message`](crate::protocol::Message),
//! and every change is authorized before it touches the live doc:
//! [`MeshDoc::ingest`] applies the change to a throwaway fork first and rejects
//! it if it would alter any peer's `/peers/<nick>/card` other than the author's
//! own — the card carries the peer's cryptographic identity, so this is the
//! automerge analogue of the old `meta_merge_forges_foreign_card` gate. Because
//! every honest member runs the same gate before applying, a forged card
//! converges nowhere.
//!
//! Changes arriving before their causal dependencies (out-of-order backfill) are
//! held in `pending` and drained — through the same gate — once their deps land,
//! so the gate is never bypassed by dependency buffering.

pub mod wire;

use std::collections::{HashMap, HashSet};

use automerge::transaction::Transactable;
use automerge::{
    AutoSerde, Automerge, Change, ChangeHash, ObjId, ObjType, ROOT, ReadDoc, Value as AmValue,
};
use serde_json::{Map, Value};

use crate::protocol::{Message, Nickname};

/// The outcome of ingesting one frame into a [`MeshDoc`].
#[derive(Debug)]
pub(crate) enum Ingested {
    /// Applied to the live doc. `changed` is whether the derived document moved
    /// (a no-op change surfaces nothing); `doc` is the document after applying
    /// this change and draining anything it unblocked.
    Applied { changed: bool, doc: Value },
    /// A change we already hold — dropped.
    Duplicate,
    /// Held pending its causal dependencies; not yet applied.
    Buffered,
    /// Refused: it would forge another peer's card. Never applied.
    Rejected,
    /// The frame body is not a decodable automerge change (a legacy/foreign
    /// body) — a no-op.
    Ignored,
}

/// One channel's automerge document plus the bookkeeping to apply changes in
/// causal order and re-serve them.
#[derive(Debug)]
pub struct MeshDoc {
    // Boxed: an `Automerge` is large inline, and two `MeshDoc`s live in the
    // event-loop state that several CLI futures capture — keeping it off the
    // stack holds those futures under clippy's `large_futures` size threshold.
    doc: Box<Automerge>,
    /// Hashes of every change applied to `doc` — the "deps satisfied?" oracle
    /// (includes the internal genesis change, which has no frame).
    applied: HashSet<ChangeHash>,
    /// The signed frame that carried each applied change, keyed by change hash —
    /// the re-serve store (a peer forwards another author's change with its
    /// original signature intact). Replaces the old `StateLog`.
    frames: HashMap<ChangeHash, Message>,
    /// Orphan frames awaiting their change's deps, keyed by change hash.
    pending: HashMap<ChangeHash, Message>,
    /// Whether this channel gates foreign-card writes (`meta` does; `state`
    /// carries no identity so it does not).
    gate_cards: bool,
    /// This channel's symmetric encryption key, on a password-protected mesh.
    /// `Some` ⇒ change bodies are sealed on the wire (`enc` envelope) and
    /// decrypted here before applying; `None` ⇒ plaintext, exactly as before.
    /// Wiped on drop.
    key: Option<zeroize::Zeroizing<[u8; 32]>>,
}

impl MeshDoc {
    #[must_use]
    pub fn new(gate_cards: bool) -> Self {
        let mut doc = Automerge::new();
        let mut applied = HashSet::new();
        // The `meta` channel's `/peers` map must have ONE object identity across
        // every replica, or two peers each vivifying `/peers` would create
        // conflicting maps and automerge would discard one — silently erasing a
        // peer's card (its identity). A byte-identical genesis change (fixed
        // actor, fixed time) gives that map a shared id everywhere; per-peer
        // writes then land in the same map as distinct keys and merge cleanly.
        if gate_cards {
            let genesis = peers_genesis();
            let hash = genesis.hash();
            let _ = doc.apply_changes([genesis]);
            applied.insert(hash);
        }
        Self {
            doc: Box::new(doc),
            applied,
            frames: HashMap::new(),
            pending: HashMap::new(),
            gate_cards,
            key: None,
        }
    }

    /// Set this channel's encryption key (the daemon's per-channel
    /// mesh-password-derived key). `None` leaves the channel in plaintext.
    #[must_use]
    pub(crate) fn with_key(mut self, key: Option<zeroize::Zeroizing<[u8; 32]>>) -> Self {
        self.key = key;
        self
    }

    /// The plaintext automerge change bytes carried by `frame`, decrypting an
    /// `enc` body with this channel's key first. `None` for an opaque/foreign
    /// body (or, on a passworded mesh, an unsealed or unopenable body) — a
    /// no-op on the doc.
    fn change_bytes(&self, frame: &Message) -> Option<Vec<u8>> {
        match self.key.as_deref() {
            // Passwordless: parse directly — no decrypt indirection, no clone.
            None => crate::daemon::state_doc::parse_change_body(frame.body.as_str()),
            Some(key) => {
                let plain = crate::daemon::state_doc::decrypt_body(frame.body.as_str(), Some(key))?;
                crate::daemon::state_doc::parse_change_body(&plain)
            }
        }
    }

    /// Compose the wire body for a locally-built change: the plaintext
    /// `change`/`merge` envelope, sealed under this channel's key when set.
    /// Returns `(wire, plain)` — `wire` is signed + gossiped + retained for
    /// re-serve; `plain` surfaces the author's own human-readable delta locally.
    ///
    /// # Errors
    /// Body serialization/size or the encryption envelope fails.
    pub(crate) fn compose_wire_body(
        &self,
        change: &[u8],
        merge: Option<&Value>,
    ) -> anyhow::Result<(crate::protocol::MessageBody, crate::protocol::MessageBody)> {
        let plain = crate::daemon::state_doc::change_body(change, merge)?;
        let wire = match self.key.as_deref() {
            Some(key) => crate::daemon::state_doc::encrypt_body(&plain, key)?,
            None => plain.clone(),
        };
        Ok((wire, plain))
    }

    /// The plaintext body string to surface for `frame`, decrypting an `enc`
    /// body so the `m` delta renders. `None` when it can't be opened; the caller
    /// then surfaces the original (opaque) frame.
    pub(crate) fn surface_body(&self, frame: &Message) -> Option<String> {
        crate::daemon::state_doc::decrypt_body(frame.body.as_str(), self.key.as_deref())
    }

    /// This channel's automerge heads, Base58-encoded — the compact frontier a
    /// peer advertises so others can compute exactly what it is missing.
    pub(crate) fn heads(&self) -> Vec<String> {
        self.doc.get_heads().iter().map(encode_hash).collect()
    }

    /// The signed frames for changes a peer with heads `have` is missing, newest
    /// causal frontier first, capped at `max`. Undecodable heads are ignored (we
    /// then over-serve, never under-serve). The genesis change has no frame and
    /// is never sent — every replica constructs it locally.
    pub(crate) fn changes_since(&self, have: &[String], max: usize) -> Vec<Message> {
        let have: Vec<ChangeHash> = have
            .iter()
            .filter_map(|encoded| decode_hash(encoded))
            .collect();
        self.doc
            .get_changes(&have)
            .into_iter()
            .filter_map(|change| self.frames.get(&change.hash()).cloned())
            .take(max)
            .collect()
    }

    /// The derived document as JSON — the shape `agent-gossip state|meta get` returns.
    #[must_use]
    pub fn to_json(&self) -> Value {
        doc_json(&self.doc)
    }

    /// Translate an RFC 7386 merge document into a single automerge change,
    /// computed against current heads **without mutating the live doc**. `None`
    /// when the merge produced no ops. The caller size-gates the resulting frame
    /// before feeding the bytes back through [`MeshDoc::ingest`] to apply them,
    /// so an oversize change never lands in the doc it could not be gossiped for.
    ///
    /// `actor_seed` must be unique per session — the daemon passes its signing
    /// public key (see [`actor_for`]).
    ///
    /// # Errors
    /// Unrepresentable merge (a non-object at the document root).
    pub fn build_change(
        &self,
        merge: &Value,
        actor_seed: &[u8],
    ) -> anyhow::Result<Option<Vec<u8>>> {
        let Value::Object(_) = merge else {
            anyhow::bail!("merge must be a JSON object (automerge's document root is a map)");
        };
        let mut fork = self.doc.fork();
        fork.set_actor(actor_for(actor_seed));
        let heads = fork.get_heads();
        {
            let mut tx = fork.transaction();
            write_map(&mut tx, &ROOT, merge)?;
            tx.commit();
        }
        let mut changes = fork.get_changes(&heads);
        Ok(changes.pop().map(|change| change.raw_bytes().to_vec()))
    }

    /// Ingest one signed frame carrying an automerge change. Verifies causal
    /// readiness and card authorization before applying; buffers orphans (as
    /// frames, so re-serve and the gate both see the original signed frame). The
    /// frame's signature is verified upstream by `gossip::ingest`.
    pub(crate) fn ingest(&mut self, frame: &Message) -> Ingested {
        let Some(bytes) = self.change_bytes(frame) else {
            return Ingested::Ignored;
        };
        let Ok(change) = Change::from_bytes(bytes) else {
            return Ingested::Ignored;
        };
        let hash = change.hash();
        if self.applied.contains(&hash) {
            return Ingested::Duplicate;
        }
        if !self.deps_satisfied(change.deps()) {
            self.pending.insert(hash, frame.clone());
            return Ingested::Buffered;
        }
        if self.gate_cards && forges_foreign_card(&self.doc, &change, &frame.author) {
            return Ingested::Rejected;
        }
        let before = self.to_json();
        self.apply(change, hash, frame.clone());
        self.drain_pending();
        let after = self.to_json();
        Ingested::Applied {
            changed: before != after,
            doc: after,
        }
    }

    fn apply(&mut self, change: Change, hash: ChangeHash, frame: Message) {
        // apply_changes only fails on missing deps, which we checked, or a
        // corrupt change, which decode already validated.
        let _ = self.doc.apply_changes([change]);
        self.applied.insert(hash);
        self.frames.insert(hash, frame);
    }

    fn deps_satisfied(&self, deps: &[ChangeHash]) -> bool {
        deps.iter().all(|dep| self.applied.contains(dep))
    }

    /// Apply every buffered frame whose change's deps are now met — through the
    /// gate — repeating until a full pass unblocks nothing.
    fn drain_pending(&mut self) {
        loop {
            let ready: Vec<ChangeHash> = self
                .pending
                .iter()
                .filter_map(|(hash, frame)| {
                    let bytes = self.change_bytes(frame)?;
                    Change::from_bytes(bytes)
                        .ok()
                        .filter(|change| self.deps_satisfied(change.deps()))
                        .map(|_| *hash)
                })
                .collect();
            if ready.is_empty() {
                return;
            }
            for hash in ready {
                self.try_apply_pending(hash);
            }
        }
    }

    /// Apply one now-ready buffered frame through the gate, dropping it
    /// silently if it's no longer pending, decodes badly, or forges a card —
    /// same handling as the equivalent direct-ingest failure modes.
    fn try_apply_pending(&mut self, hash: ChangeHash) {
        let Some(frame) = self.pending.remove(&hash) else {
            return;
        };
        let Some(bytes) = self.change_bytes(&frame) else {
            return;
        };
        let Ok(change) = Change::from_bytes(bytes) else {
            return;
        };
        if self.gate_cards && forges_foreign_card(&self.doc, &change, &frame.author) {
            return; // dropped, same as a directly-rejected change
        }
        self.apply(change, hash, frame);
    }
}

/// Base58-encode an automerge change hash for a `MessageBody`-safe wire form.
fn encode_hash(hash: &ChangeHash) -> String {
    bs58::encode(hash.0).into_string()
}

/// Decode a Base58 change hash; `None` if it isn't 32 valid Base58 bytes.
fn decode_hash(encoded: &str) -> Option<ChangeHash> {
    let bytes = bs58::decode(encoded).into_vec().ok()?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok().map(ChangeHash)
}

/// Would applying `change` (authored by `author`) alter any peer's card other
/// than the author's own? Applied to a throwaway fork so the live doc is never
/// touched by an unauthorized change.
fn forges_foreign_card(base: &Automerge, change: &Change, author: &Nickname) -> bool {
    let mut fork = base.fork();
    if fork.apply_changes([change.clone()]).is_err() {
        return true;
    }
    let before = peer_cards(base);
    let after = peer_cards(&fork);
    before
        .keys()
        .chain(after.keys())
        .any(|nick| nick.as_str() != author.as_str() && before.get(nick) != after.get(nick))
}

/// Each peer's `/peers/<nick>/card` subtree, keyed by nick (absent → not in the
/// map). Derived from the hydrated JSON so it captures the card whatever its
/// shape.
fn peer_cards(doc: &Automerge) -> Map<String, Value> {
    let json = doc_json(doc);
    let mut cards = Map::new();
    if let Some(peers) = json.get("peers").and_then(Value::as_object) {
        for (nick, entry) in peers {
            if let Some(card) = entry.get("card") {
                cards.insert(nick.clone(), card.clone());
            }
        }
    }
    cards
}

fn doc_json(doc: &Automerge) -> Value {
    serde_json::to_value(AutoSerde::from(doc)).unwrap_or(Value::Null)
}

/// The deterministic genesis change that creates the shared `/peers` map. Built
/// from constants (fixed actor, `time = 0`) so its hash — and thus the map's
/// object id — is identical on every replica.
fn peers_genesis() -> Change {
    let mut doc = Automerge::new();
    doc.set_actor(automerge::ActorId::from(b"agent-gossip/genesis".as_slice()));
    {
        let mut tx = doc.transaction();
        tx.put_object(&ROOT, "peers", ObjType::Map)
            .expect("root is a map");
        tx.commit_with(automerge::transaction::CommitOptions::default().with_time(0));
    }
    doc.get_changes(&[])
        .pop()
        .expect("genesis produced exactly one change")
}

/// Derive a stable automerge [`ActorId`](automerge::ActorId) from a
/// per-session seed (the daemon passes its signing public key) so a member's
/// concurrent same-key writes resolve deterministically. Authenticity is the
/// signed envelope's job, not the actor id — this only stabilizes automerge's
/// own conflict tie-break.
///
/// The seed must NOT be the nickname: automerge numbers each actor's changes
/// sequentially, so a rejoined session (fresh doc, seq restarting at 1) under
/// a nickname-derived actor collides with its predecessor's history and every
/// replica rejects its writes as `DuplicateSeqNumber` equivocation. The
/// session identity is minted fresh per run, which makes it exactly
/// session-unique. (If identity ever persists across restarts, the seed must
/// grow a per-run component, since the docs start empty each run.)
fn actor_for(seed: &[u8]) -> automerge::ActorId {
    automerge::ActorId::from(seed)
}

/// Apply an RFC 7386 object merge into the map `obj`: recurse into object
/// members (vivifying missing maps), delete on `null`, and replace on any
/// scalar or array.
fn write_map(
    tx: &mut automerge::transaction::Transaction<'_>,
    obj: &ObjId,
    merge: &Value,
) -> anyhow::Result<()> {
    let Value::Object(map) = merge else {
        // Non-object nested merge is handled by the caller (write_value); the
        // root is guaranteed an object by build_change.
        return Ok(());
    };
    for (key, value) in map {
        match value {
            Value::Null => {
                let _ = tx.delete(obj, key.as_str());
            }
            Value::Object(_) => {
                let child = ensure_map(tx, obj, key)?;
                write_map(tx, &child, value)?;
            }
            Value::Bool(_) | Value::String(_) | Value::Number(_) | Value::Array(_) => {
                write_value(tx, obj, key, value)?;
            }
        }
    }
    Ok(())
}

/// The map object at `obj[key]`, reusing an existing map or creating one.
fn ensure_map(
    tx: &mut automerge::transaction::Transaction<'_>,
    obj: &ObjId,
    key: &str,
) -> anyhow::Result<ObjId> {
    if let Some((AmValue::Object(ObjType::Map), id)) = tx.get(obj, key)? {
        return Ok(id);
    }
    Ok(tx.put_object(obj, key, ObjType::Map)?)
}

/// Write a scalar or array as `obj[key]`, replacing whatever is there (RFC 7386
/// semantics for non-object values).
fn write_value(
    tx: &mut automerge::transaction::Transaction<'_>,
    obj: &ObjId,
    key: &str,
    value: &Value,
) -> anyhow::Result<()> {
    match value {
        Value::Array(items) => {
            let list = tx.put_object(obj, key, ObjType::List)?;
            for (index, item) in items.iter().enumerate() {
                append_item(tx, &list, index, item)?;
            }
        }
        Value::Object(_) | Value::Null => unreachable!("handled by write_map"),
        Value::Bool(_) | Value::String(_) | Value::Number(_) => put_scalar(tx, obj, key, value)?,
    }
    Ok(())
}

fn append_item(
    tx: &mut automerge::transaction::Transaction<'_>,
    list: &ObjId,
    index: usize,
    item: &Value,
) -> anyhow::Result<()> {
    match item {
        Value::Object(_) => {
            let child = tx.insert_object(list, index, ObjType::Map)?;
            write_map(tx, &child, item)?;
        }
        Value::Array(inner) => {
            let child = tx.insert_object(list, index, ObjType::List)?;
            for (inner_index, inner_item) in inner.iter().enumerate() {
                append_item(tx, &child, inner_index, inner_item)?;
            }
        }
        Value::Bool(_) | Value::String(_) | Value::Number(_) | Value::Null => {
            insert_scalar(tx, list, index, item)?;
        }
    }
    Ok(())
}

fn put_scalar(
    tx: &mut automerge::transaction::Transaction<'_>,
    obj: &ObjId,
    key: &str,
    scalar: &Value,
) -> anyhow::Result<()> {
    match scalar {
        Value::Bool(bool_value) => tx.put(obj, key, *bool_value)?,
        Value::String(string_value) => tx.put(obj, key, string_value.as_str())?,
        Value::Number(number) => put_number(tx, obj, key, number)?,
        Value::Null | Value::Array(_) | Value::Object(_) => {
            unreachable!("non-scalar handled elsewhere")
        }
    }
    Ok(())
}

fn insert_scalar(
    tx: &mut automerge::transaction::Transaction<'_>,
    list: &ObjId,
    index: usize,
    scalar: &Value,
) -> anyhow::Result<()> {
    match scalar {
        Value::Bool(bool_value) => tx.insert(list, index, *bool_value)?,
        Value::String(string_value) => tx.insert(list, index, string_value.as_str())?,
        Value::Number(number) => {
            if let Some(int_value) = number.as_i64() {
                tx.insert(list, index, int_value)?;
            } else if let Some(uint_value) = number.as_u64() {
                tx.insert(list, index, uint_value)?;
            } else if let Some(float_value) = number.as_f64() {
                tx.insert(list, index, float_value)?;
            }
        }
        // A JSON array element may be `null` (unlike a map, where `null` means
        // delete and is handled upstream), so a list preserves it as a null.
        Value::Null => tx.insert(list, index, automerge::ScalarValue::Null)?,
        Value::Array(_) | Value::Object(_) => unreachable!("non-scalar handled elsewhere"),
    }
    Ok(())
}

fn put_number(
    tx: &mut automerge::transaction::Transaction<'_>,
    obj: &ObjId,
    key: &str,
    number: &serde_json::Number,
) -> anyhow::Result<()> {
    if let Some(int_value) = number.as_i64() {
        tx.put(obj, key, int_value)?;
    } else if let Some(uint_value) = number.as_u64() {
        tx.put(obj, key, uint_value)?;
    } else if let Some(float_value) = number.as_f64() {
        tx.put(obj, key, float_value)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Ingested, MeshDoc};
    use crate::daemon::state_doc::change_body;
    use crate::protocol::{Channel, MeshId, Message, Nickname};
    use serde_json::{Value, json};

    fn nick(name: &str) -> Nickname {
        Nickname::from(name)
    }

    /// Wrap change bytes in a signed-frame stand-in (the doc layer reads only
    /// `author` + `body`; a valid signature is `gossip::ingest`'s job).
    fn frame(who: &Nickname, bytes: &[u8]) -> Message {
        Message::new_channel_event(
            &MeshId::from("💬test"),
            who,
            change_body(bytes, None).expect("body"),
            Channel::State,
        )
    }

    /// Author a merge on `doc` (build + ingest, as the daemon does) and return
    /// the signed frame to hand to a peer. The actor seed is the nickname —
    /// fine for single-session tests; a test spanning two sessions of one
    /// nickname must use [`author_as`] with distinct seeds.
    fn author(doc: &mut MeshDoc, who: &Nickname, merge: &Value) -> Message {
        author_as(doc, who, who.as_str().as_bytes(), merge)
    }

    /// [`author`] with an explicit per-session actor seed, as the daemon
    /// derives from its signing key.
    fn author_as(doc: &mut MeshDoc, who: &Nickname, seed: &[u8], merge: &Value) -> Message {
        let bytes = doc
            .build_change(merge, seed)
            .expect("merge applies")
            .expect("merge is not a no-op");
        let carrier = frame(who, &bytes);
        let outcome = doc.ingest(&carrier);
        assert!(
            matches!(outcome, Ingested::Applied { .. }),
            "expected applied, got {outcome:?}"
        );
        carrier
    }

    #[test]
    fn distinct_top_level_keys_converge_either_order() {
        // Distinct keys at the always-shared document root merge with no genesis.
        let (alice, bob) = (nick("alice"), nick("bob"));
        let mut left = MeshDoc::new(false);
        let mut right = MeshDoc::new(false);

        let alice_frame = author(&mut left, &alice, &json!({"a": 1}));
        let bob_frame = author(&mut right, &bob, &json!({"b": 2}));

        left.ingest(&bob_frame);
        right.ingest(&alice_frame);

        let want = json!({"a": 1, "b": 2});
        assert_eq!(left.to_json(), want);
        assert_eq!(right.to_json(), want);
    }

    #[test]
    fn concurrent_peer_reports_converge_via_shared_genesis() {
        // Two peers each vivify their own `/peers/<nick>` entry concurrently. The
        // shared `/peers` genesis makes these distinct keys in one map, so both
        // survive — the case that erased a card before the genesis existed.
        let (alice, bob) = (nick("alice"), nick("bob"));
        let mut left = MeshDoc::new(true);
        let mut right = MeshDoc::new(true);

        let alice_frame = author(&mut left, &alice, &json!({"peers": {"alice": {"m": 1}}}));
        let bob_frame = author(&mut right, &bob, &json!({"peers": {"bob": {"m": 2}}}));

        left.ingest(&bob_frame);
        right.ingest(&alice_frame);

        let want = json!({"peers": {"alice": {"m": 1}, "bob": {"m": 2}}});
        assert_eq!(left.to_json(), want);
        assert_eq!(right.to_json(), want);
    }

    #[test]
    fn null_deletes_key_and_preserves_siblings() {
        let alice = nick("alice");
        let mut doc = MeshDoc::new(false);
        author(
            &mut doc,
            &alice,
            &json!({"peers": {"alice": {"model": "opus", "host": "box"}}}),
        );
        author(
            &mut doc,
            &alice,
            &json!({"peers": {"alice": {"model": "sonnet"}}}),
        );
        author(
            &mut doc,
            &alice,
            &json!({"peers": {"alice": {"host": null}}}),
        );
        assert_eq!(
            doc.to_json(),
            json!({"peers": {"alice": {"model": "sonnet"}}})
        );
    }

    #[test]
    fn out_of_order_change_is_buffered_then_drains() {
        let alice = nick("alice");
        let mut source = MeshDoc::new(false);
        let first = author(&mut source, &alice, &json!({"a": 1}));
        let second = author(&mut source, &alice, &json!({"b": 2}));

        // Deliver the second change first: it depends on the first, so it buffers.
        let mut sink = MeshDoc::new(false);
        assert!(matches!(sink.ingest(&second), Ingested::Buffered));
        assert_eq!(sink.to_json(), json!({}));
        // The first change unblocks the buffered second in one ingest.
        assert!(matches!(sink.ingest(&first), Ingested::Applied { .. }));
        assert_eq!(sink.to_json(), json!({"a": 1, "b": 2}));
    }

    #[test]
    fn heads_and_changes_since_reconcile_a_late_joiner() {
        // A source authors two changes; a fresh joiner advertises its (empty)
        // heads and pulls exactly the frames it lacks, then converges.
        let alice = nick("alice");
        let mut source = MeshDoc::new(false);
        author(&mut source, &alice, &json!({"a": 1}));
        author(&mut source, &alice, &json!({"b": 2}));

        let mut joiner = MeshDoc::new(false);
        let missing = source.changes_since(&joiner.heads(), 100);
        assert_eq!(missing.len(), 2, "joiner lacks both changes");
        for carrier in &missing {
            joiner.ingest(carrier);
        }
        assert_eq!(joiner.to_json(), json!({"a": 1, "b": 2}));
        // Now converged, the source has nothing more to offer.
        assert!(source.changes_since(&joiner.heads(), 100).is_empty());
    }

    #[test]
    fn encrypted_change_converges_with_the_key_and_is_opaque_without() {
        let alice = nick("alice");
        let key = [42u8; 32];
        // Author builds the change and seals the wire body, as the daemon does.
        let merge = json!({"secret": "value"});
        let author_doc = MeshDoc::new(false).with_key(Some(zeroize::Zeroizing::new(key)));
        let bytes = author_doc
            .build_change(&merge, alice.as_str().as_bytes())
            .expect("builds")
            .expect("not a no-op");
        let (wire, _plain) = author_doc
            .compose_wire_body(&bytes, Some(&merge))
            .expect("compose");
        let carrier =
            Message::new_channel_event(&MeshId::from("💬test"), &alice, wire, Channel::State);
        assert!(
            !carrier.body.as_str().contains("value"),
            "the plaintext value must not appear on the wire"
        );

        // A peer holding the key applies it and converges; surface_body recovers
        // the plaintext for the delta.
        let mut with_key = MeshDoc::new(false).with_key(Some(zeroize::Zeroizing::new(key)));
        assert!(matches!(
            with_key.ingest(&carrier),
            Ingested::Applied { .. }
        ));
        assert_eq!(with_key.to_json(), json!({"secret": "value"}));
        assert!(with_key.surface_body(&carrier).unwrap().contains("secret"));

        // A peer without the key (or with the wrong key) cannot read it — the
        // body is an opaque no-op, never applied.
        let mut no_key = MeshDoc::new(false);
        assert!(matches!(no_key.ingest(&carrier), Ingested::Ignored));
        assert_eq!(no_key.to_json(), json!({}));
        let mut wrong_key = MeshDoc::new(false).with_key(Some(zeroize::Zeroizing::new([7u8; 32])));
        assert!(matches!(wrong_key.ingest(&carrier), Ingested::Ignored));
        assert_eq!(wrong_key.to_json(), json!({}));
    }

    #[test]
    fn foreign_card_write_is_rejected_own_is_allowed() {
        let (alice, bob) = (nick("alice"), nick("bob"));

        // Craft both changes on one ungated replica so the forge is built atop
        // Bob's card (deps satisfied) — what an attacker who has synced holds.
        let mut attacker = MeshDoc::new(false);
        let bob_card = author(
            &mut attacker,
            &bob,
            &json!({"peers": {"bob": {"card": {"metadata": {"pubkey": "bb"}}}}}),
        );
        let forged = author(
            &mut attacker,
            &alice,
            &json!({"peers": {"bob": {"card": {"metadata": {"pubkey": "ff"}}}}}),
        );

        // A gated victim accepts Bob's real card, then refuses Alice's forge.
        let mut victim = MeshDoc::new(true);
        assert!(matches!(victim.ingest(&bob_card), Ingested::Applied { .. }));
        assert!(matches!(victim.ingest(&forged), Ingested::Rejected));
        assert_eq!(
            victim.to_json(),
            json!({"peers": {"bob": {"card": {"metadata": {"pubkey": "bb"}}}}})
        );

        // Alice writing her OWN card through the gate is allowed.
        let mut alice_doc = MeshDoc::new(true);
        let own_bytes = alice_doc
            .build_change(
                &json!({"peers": {"alice": {"card": {"name": "alice"}}}}),
                alice.as_str().as_bytes(),
            )
            .expect("builds")
            .expect("not a no-op");
        assert!(matches!(
            alice_doc.ingest(&frame(&alice, &own_bytes)),
            Ingested::Applied { .. }
        ));
    }

    #[test]
    fn self_entry_delete_passes_gate_foreign_delete_is_rejected() {
        // The leave-time retraction: a peer nulls its own `/peers/<nick>`
        // entry. The gate must let the owner do it and refuse anyone else —
        // which is why departed peers can only ever be pruned by themselves.
        let (alice, bob) = (nick("alice"), nick("bob"));

        // Craft on one ungated replica so deps are satisfied: alice's card,
        // then alice's own retraction.
        let mut source = MeshDoc::new(false);
        let alice_card = author(
            &mut source,
            &alice,
            &json!({"peers": {"alice": {"card": {"name": "alice"}, "model": "m1"}}}),
        );
        let self_delete = author(&mut source, &alice, &json!({"peers": {"alice": null}}));

        let mut victim = MeshDoc::new(true);
        assert!(matches!(victim.ingest(&alice_card), Ingested::Applied { .. }));
        assert!(matches!(
            victim.ingest(&self_delete),
            Ingested::Applied { .. }
        ));
        assert_eq!(
            victim.to_json().pointer("/peers/alice"),
            None,
            "the whole entry — card and agent facts — is gone"
        );

        // The same delete authored by bob alters alice's card → rejected, and
        // alice's entry survives on the gated replica.
        let mut foreign_source = MeshDoc::new(false);
        let card_frame = author(
            &mut foreign_source,
            &alice,
            &json!({"peers": {"alice": {"card": {"name": "alice"}}}}),
        );
        let foreign_delete = author(
            &mut foreign_source,
            &bob,
            &json!({"peers": {"alice": null}}),
        );
        let mut gated = MeshDoc::new(true);
        assert!(matches!(gated.ingest(&card_frame), Ingested::Applied { .. }));
        assert!(matches!(gated.ingest(&foreign_delete), Ingested::Rejected));
        assert_eq!(
            gated.to_json().pointer("/peers/alice/card/name"),
            Some(&json!("alice"))
        );
    }

    /// The rejoin-after-retraction shape: a replica holds a peer's card and
    /// its self-delete; a *fresh* session (same nickname, new signing key →
    /// new actor seed, no shared history beyond genesis) publishes a new
    /// card. The concurrent re-add must survive the merge on both sides — a
    /// departed nickname is never burned. This is exactly why the actor seed
    /// is the session key, not the nickname: a nickname-derived actor makes
    /// the new session's seq-1 change collide with the old session's and
    /// every replica rejects it as `DuplicateSeqNumber` equivocation.
    #[test]
    fn fresh_republish_survives_a_prior_self_delete() {
        let alice = nick("alice");

        // Old session: card, then the leave-time retraction.
        let mut old_session = MeshDoc::new(true);
        let old_card = author_as(
            &mut old_session,
            &alice,
            b"alice-session-key-old",
            &json!({"peers": {"alice": {"card": {"name": "alice", "session": "old"}}}}),
        );
        let retraction = author_as(
            &mut old_session,
            &alice,
            b"alice-session-key-old",
            &json!({"peers": {"alice": null}}),
        );

        // A staying peer applied both.
        let mut host = MeshDoc::new(true);
        assert!(matches!(host.ingest(&old_card), Ingested::Applied { .. }));
        assert!(matches!(host.ingest(&retraction), Ingested::Applied { .. }));
        assert_eq!(host.to_json().pointer("/peers/alice"), None);

        // New session: fresh doc, fresh key, deps = genesis only.
        let mut new_session = MeshDoc::new(true);
        let new_card = author_as(
            &mut new_session,
            &alice,
            b"alice-session-key-new",
            &json!({"peers": {"alice": {"card": {"name": "alice", "session": "new"}}}}),
        );

        // Both directions converge on the new card.
        let outcome = host.ingest(&new_card);
        assert!(
            matches!(outcome, Ingested::Applied { .. }),
            "expected applied, got {outcome:?}"
        );
        assert_eq!(
            host.to_json().pointer("/peers/alice/card/session"),
            Some(&json!("new")),
            "the fresh publish must survive the earlier delete: {}",
            host.to_json()
        );
        assert!(matches!(
            new_session.ingest(&old_card),
            Ingested::Applied { .. }
        ));
        assert!(matches!(
            new_session.ingest(&retraction),
            Ingested::Applied { .. }
        ));
        assert_eq!(
            new_session.to_json().pointer("/peers/alice/card/session"),
            Some(&json!("new")),
            "backfilling the old history must not erase the new card: {}",
            new_session.to_json()
        );
    }
}
