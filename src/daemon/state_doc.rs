//! The shared-state document layer: the `State` message-body schema and the
//! RFC 7386 JSON Merge Patch reducer that folds the state log into a single JSON
//! document. Phase 1 carries only merges; snapshots/compaction are Phase 2.
//!
//! Each body is a single **merge document** (any JSON value). Applying it is a
//! recursive deep-merge (RFC 7386 §2): an object recurses key-by-key (a `null`
//! member deletes that key, a missing object is created on the way down), and any
//! non-object value (a scalar, array, or `null`) replaces the target. So a peer
//! writes only its own keys — `{"peers":{"<nick>":{…}}}` creates `/peers` if
//! absent and sets just that entry, never clobbering a sibling — while a
//! top-level non-object merge replaces the whole document, exactly as the spec
//! prescribes (no added validation, no special-casing).
//!
//! Merge is **not** commutative, but every peer folds the same log in the same
//! deterministic `(timestamp, id)` order ([`crate::daemon::state_log`]), so all
//! peers compute a byte-identical document — the convergence guarantee. A body
//! whose envelope tag doesn't parse (e.g. an old binary's RFC 6902 `patch` body)
//! is a **deterministic no-op**.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::daemon::state_log::{StateLog, StateProjection};
use crate::protocol::Message;
use crate::protocol::message::MessageBody;

/// The tagged `State` message body. Phase 1 has only `Merge`; `Snapshot` arrives
/// with compaction. An unknown tag fails to parse and is ignored by the reducer
/// (forward-compatible — and what an old `{"k":"patch",…}` body now hits).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
enum StateOp {
    Merge { merge: Value },
}

/// Compose the `MessageBody` for a state change from its RFC 7386 merge document.
pub(crate) fn merge_body(merge: Value) -> anyhow::Result<MessageBody> {
    let json = serde_json::to_string(&StateOp::Merge { merge })?;
    MessageBody::new(json).map_err(|error| anyhow::anyhow!("{error}"))
}

/// RFC 7386 §2: recursively merge `patch` into `target`. An object patch recurses
/// key-by-key (a `null` member removes that key, vivifying a missing object on the
/// way down); any non-object patch replaces the target wholesale.
fn merge_into(target: &mut Value, patch: &Value) {
    let Value::Object(patch_map) = patch else {
        *target = patch.clone();
        return;
    };
    // RFC 7386: if the target isn't an object, start the merge from an empty one.
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    let target_map = target
        .as_object_mut()
        .expect("target was just set to an object");
    for (key, sub) in patch_map {
        if sub.is_null() {
            target_map.remove(key);
        } else {
            merge_into(target_map.entry(key.clone()).or_insert(Value::Null), sub);
        }
    }
}

/// The reducer hook ([`StateProjection::apply`]): fold one merge body into `doc`.
/// A body whose envelope tag doesn't parse is a no-op (forward-compat); otherwise
/// the merge value (any JSON) is applied per RFC 7386.
fn apply_merge_body(doc: &mut Value, body: &str) {
    let Ok(StateOp::Merge { merge }) = serde_json::from_str::<StateOp>(body) else {
        return;
    };
    merge_into(doc, &merge);
}

/// The reducer: replays the state log's merges (in its deterministic
/// `(timestamp, id)` order) into one JSON document.
#[derive(Debug)]
struct JsonDoc {
    value: Value,
}

impl Default for JsonDoc {
    fn default() -> Self {
        Self {
            value: Value::Object(Map::new()),
        }
    }
}

impl StateProjection for JsonDoc {
    fn apply(&mut self, event: &Message) {
        apply_merge_body(&mut self.value, event.body.as_str());
    }
}

/// Fold the whole state log into the current derived document.
pub(crate) fn derive_document(state_log: &StateLog) -> Value {
    let mut doc = JsonDoc::default();
    state_log.derive(&mut doc);
    doc.value
}

#[cfg(test)]
mod tests {
    use super::{derive_document, merge_body};
    use crate::daemon::state_log::StateLog;
    use crate::protocol::{Message, Nickname, SwarmId};
    use serde_json::{Value, json};

    fn fixture() -> (SwarmId, Nickname) {
        (SwarmId::from("💬test"), Nickname::from("test-node"))
    }

    /// Build a `State` merge event with a fixed timestamp so replay order is
    /// deterministic in tests.
    fn merge_event(swarm: &SwarmId, author: &Nickname, ts: i64, merge: Value) -> Message {
        let mut msg = Message::new_state(swarm, author, merge_body(merge).unwrap());
        msg.timestamp = ts;
        msg
    }

    #[test]
    fn merge_folds_distinct_keys_order_independently() {
        // The reported bug: peers reporting into a shared `/peers` map must all
        // converge, in any replay order, with no peer clobbering another. Distinct
        // keys commute, so forward and reverse insert order yield the same doc.
        let swarm = SwarmId::from("💬test");
        let (alice, bob, carol) = (
            Nickname::from("alice"),
            Nickname::from("bob"),
            Nickname::from("carol"),
        );
        let merges = [
            (10, &alice, json!({"peers": {"alice": {"m": 1}}})),
            (20, &bob, json!({"peers": {"bob": {"m": 2}}})),
            (30, &carol, json!({"peers": {"carol": {"m": 3}}})),
        ];
        let mut forward = StateLog::new();
        let mut reverse = StateLog::new();
        for (ts, author, merge) in &merges {
            forward.insert(merge_event(&swarm, author, *ts, merge.clone()));
        }
        for (ts, author, merge) in merges.iter().rev() {
            reverse.insert(merge_event(&swarm, author, *ts, merge.clone()));
        }
        let expected = json!({"peers": {"alice": {"m":1}, "bob": {"m":2}, "carol": {"m":3}}});
        assert_eq!(derive_document(&forward), expected);
        assert_eq!(derive_document(&reverse), expected);
    }

    #[test]
    fn merge_auto_creates_container() {
        // A per-key report into a map nobody has created yet still applies: the
        // merge vivifies the missing `/peers` object on the way down.
        let (swarm, author) = fixture();
        let mut log = StateLog::new();
        log.insert(merge_event(
            &swarm,
            &author,
            10,
            json!({"peers": {"alice": {"model": "opus"}}}),
        ));
        assert_eq!(
            derive_document(&log),
            json!({"peers": {"alice": {"model": "opus"}}})
        );
    }

    #[test]
    fn merge_null_deletes_key() {
        let (swarm, author) = fixture();
        let mut log = StateLog::new();
        log.insert(merge_event(
            &swarm,
            &author,
            10,
            json!({"peers": {"alice": {"m": 1}, "bob": {"m": 2}}}),
        ));
        log.insert(merge_event(
            &swarm,
            &author,
            20,
            json!({"peers": {"alice": null}}),
        ));
        assert_eq!(derive_document(&log), json!({"peers": {"bob": {"m": 2}}}));
    }

    #[test]
    fn merge_partial_update_preserves_siblings() {
        // Updating one field of a sub-object leaves the others intact (deep merge),
        // so a model switch needn't resend host/harness.
        let (swarm, author) = fixture();
        let mut log = StateLog::new();
        log.insert(merge_event(
            &swarm,
            &author,
            10,
            json!({"peers": {"alice": {"model": "opus", "host": "box"}}}),
        ));
        log.insert(merge_event(
            &swarm,
            &author,
            20,
            json!({"peers": {"alice": {"model": "sonnet"}}}),
        ));
        assert_eq!(
            derive_document(&log),
            json!({"peers": {"alice": {"model": "sonnet", "host": "box"}}})
        );
    }

    #[test]
    fn merge_same_key_resolves_in_timestamp_order() {
        // Merge is not commutative: for the SAME key, the later (timestamp, id)
        // wins — and it does so regardless of the order the log was populated in.
        let (swarm, author) = fixture();
        let early = (10, json!({"n": 1}));
        let late = (20, json!({"n": 2}));
        let mut forward = StateLog::new();
        forward.insert(merge_event(&swarm, &author, early.0, early.1.clone()));
        forward.insert(merge_event(&swarm, &author, late.0, late.1.clone()));
        let mut reverse = StateLog::new();
        reverse.insert(merge_event(&swarm, &author, late.0, late.1));
        reverse.insert(merge_event(&swarm, &author, early.0, early.1));
        assert_eq!(derive_document(&forward), json!({"n": 2}));
        assert_eq!(derive_document(&reverse), json!({"n": 2}));
    }

    #[test]
    fn non_object_merge_replaces_the_document() {
        // RFC 7386 §2: a non-object merge (scalar, array, or null) replaces the
        // target — including the document root. No guard, no special-casing.
        let (swarm, author) = fixture();
        for (merge, want) in [
            (json!([1, 2, 3]), json!([1, 2, 3])),
            (json!("scalar"), json!("scalar")),
            (json!(42), json!(42)),
            (json!(null), json!(null)),
        ] {
            let mut log = StateLog::new();
            log.insert(merge_event(&swarm, &author, 10, json!({"turn": "a"})));
            log.insert(merge_event(&swarm, &author, 20, merge));
            assert_eq!(derive_document(&log), want);
        }
        // And a later object merge folds back onto the replaced root (an object
        // patch onto a non-object target starts from an empty object).
        let mut log = StateLog::new();
        log.insert(merge_event(&swarm, &author, 10, json!("scalar")));
        log.insert(merge_event(&swarm, &author, 20, json!({"turn": "b"})));
        assert_eq!(derive_document(&log), json!({"turn": "b"}));
    }
}
