//! The shared-state document layer: the `State` message-body schema and the
//! frozen-subset JSON-Patch reducer that folds the state log into a single JSON
//! document. Phase 1 carries only patches; snapshots/compaction are Phase 2.
//!
//! The frozen op subset — `add`/`replace`/`remove` on object members plus
//! `add "/arr/-"` (append) — is a deliberate, version-stable wire contract. It
//! sidesteps the RFC 6902 corners (`test`/`move`/`copy`, numeric array indices,
//! root replacement) where independent implementations disagree, so every peer
//! folds an identical document. Out-of-subset, malformed, or non-applying
//! patches are **deterministic no-ops** — every honest peer makes the same
//! decision, keeping the fold convergent.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::daemon::state_log::{StateLog, StateProjection};
use crate::protocol::Message;
use crate::protocol::message::MessageBody;

/// The tagged `State` message body. Phase 1 has only `Patch`; `Snapshot`
/// arrives with compaction. An unknown tag fails to parse and is ignored by the
/// reducer (forward-compatible).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
enum StateOp {
    Patch { ops: Value },
}

/// Compose the `MessageBody` for a state patch from its RFC 6902 op array.
pub(crate) fn patch_body(ops: Value) -> anyhow::Result<MessageBody> {
    let json = serde_json::to_string(&StateOp::Patch { ops })?;
    MessageBody::new(json).map_err(|error| anyhow::anyhow!("{error}"))
}

/// The frozen subset, parsed from the raw op array. Anything outside it
/// (`test`/`move`/`copy`, numeric array index, root path, malformed op) makes
/// `parse_ops` return `None`, which callers treat as a whole-patch no-op.
#[derive(Debug)]
enum FrozenOp {
    Add { path: String, value: Value },
    Replace { path: String, value: Value },
    Remove { path: String },
}

impl FrozenOp {
    fn path(&self) -> &str {
        match self {
            FrozenOp::Add { path, .. }
            | FrozenOp::Replace { path, .. }
            | FrozenOp::Remove { path } => path,
        }
    }
}

/// Why a single frozen op did not apply. Distinguishes a *permanent* rejection
/// (a numeric array index — never valid in the frozen subset) from a *transient*
/// one (a missing key — a peer may add it later), so the write boundary can tell
/// the author whether re-reading and retrying could ever help.
#[derive(Debug, Clone, Copy)]
enum ApplyReject {
    /// A numeric (non-`-`) index into an array — out of the frozen subset.
    ArrayIndex,
    /// `replace`/`remove` of a key that isn't present (or a missing parent).
    MissingKey,
    /// The parent exists but is neither an object nor an array-append target.
    NotAnObject,
    /// The path has no `/` separator.
    BadPath,
}

impl ApplyReject {
    fn message(self, path: &str) -> String {
        match self {
            ApplyReject::ArrayIndex => format!(
                "numeric array indices aren't supported by the frozen patch subset: path \"{path}\". \
                 Replace the whole array at its parent path, or model the collection as an object \
                 keyed by index (e.g. {{\"0\":…,\"1\":…}}) so each element is an object path."
            ),
            ApplyReject::MissingKey => format!(
                "patch does not apply to the current shared state: nothing at \"{path}\" to change \
                 (re-read the state and retry)."
            ),
            ApplyReject::NotAnObject => {
                format!("patch does not apply: the parent of \"{path}\" is not an object.")
            }
            ApplyReject::BadPath => format!("invalid patch path \"{path}\" (must contain '/')."),
        }
    }
}

fn parse_ops(ops: &Value) -> Option<Vec<FrozenOp>> {
    let arr = ops.as_array()?;
    let mut parsed = Vec::with_capacity(arr.len());
    for op in arr {
        let obj = op.as_object()?;
        let path = obj.get("path")?.as_str()?.to_owned();
        // Forbid the root path: a root replace/remove would change the
        // document's *type* and brick every later object-path op.
        if path.is_empty() {
            return None;
        }
        match obj.get("op")?.as_str()? {
            "add" => parsed.push(FrozenOp::Add {
                path,
                value: obj.get("value")?.clone(),
            }),
            "replace" => parsed.push(FrozenOp::Replace {
                path,
                value: obj.get("value")?.clone(),
            }),
            "remove" => parsed.push(FrozenOp::Remove { path }),
            // test / move / copy / unknown — out of subset.
            _ => return None,
        }
    }
    Some(parsed)
}

/// Split `/a/b` into (`/a`, `b`), the last token RFC 6901-unescaped. `None`
/// only for a path with no `/` — every frozen path starts with one.
fn split_parent(path: &str) -> Option<(&str, String)> {
    let idx = path.rfind('/')?;
    Some((&path[..idx], unescape(&path[idx + 1..])))
}

fn unescape(token: &str) -> String {
    // RFC 6901: `~1` → `/`, then `~0` → `~` (order matters).
    token.replace("~1", "/").replace("~0", "~")
}

/// Apply one frozen op to `doc`. `Err` means it does not apply (with the reason);
/// the caller then discards the whole patch (atomic no-op).
fn apply_one(doc: &mut Value, op: &FrozenOp) -> Result<(), ApplyReject> {
    match op {
        FrozenOp::Add { path, value } => {
            let (parent_ptr, key) = split_parent(path).ok_or(ApplyReject::BadPath)?;
            let parent = doc.pointer_mut(parent_ptr).ok_or(ApplyReject::MissingKey)?;
            if let Some(map) = parent.as_object_mut() {
                map.insert(key, value.clone());
                Ok(())
            } else if let Some(arr) = parent.as_array_mut() {
                // Append only — numeric indices are out of subset.
                if key == "-" {
                    arr.push(value.clone());
                    Ok(())
                } else {
                    Err(ApplyReject::ArrayIndex)
                }
            } else {
                Err(ApplyReject::NotAnObject)
            }
        }
        FrozenOp::Replace { path, value } => {
            let (parent_ptr, key) = split_parent(path).ok_or(ApplyReject::BadPath)?;
            let parent = doc.pointer_mut(parent_ptr).ok_or(ApplyReject::MissingKey)?;
            if let Some(map) = parent.as_object_mut() {
                if map.contains_key(&key) {
                    map.insert(key, value.clone());
                    Ok(())
                } else {
                    Err(ApplyReject::MissingKey)
                }
            } else if parent.is_array() {
                Err(ApplyReject::ArrayIndex)
            } else {
                Err(ApplyReject::NotAnObject)
            }
        }
        FrozenOp::Remove { path } => {
            let (parent_ptr, key) = split_parent(path).ok_or(ApplyReject::BadPath)?;
            let parent = doc.pointer_mut(parent_ptr).ok_or(ApplyReject::MissingKey)?;
            if let Some(map) = parent.as_object_mut() {
                if map.remove(&key).is_some() {
                    Ok(())
                } else {
                    Err(ApplyReject::MissingKey)
                }
            } else if parent.is_array() {
                Err(ApplyReject::ArrayIndex)
            } else {
                Err(ApplyReject::NotAnObject)
            }
        }
    }
}

/// Apply a patch body to `doc` **atomically**: parse, validate the frozen
/// subset, apply every op to a working copy, commit only if all succeed. Any
/// failure (bad tag, out-of-subset op, non-applying op) leaves `doc` unchanged.
fn apply_patch_body(doc: &mut Value, body: &str) {
    let Ok(StateOp::Patch { ops }) = serde_json::from_str::<StateOp>(body) else {
        return;
    };
    let Some(frozen) = parse_ops(&ops) else {
        return;
    };
    let mut working = doc.clone();
    for op in &frozen {
        if apply_one(&mut working, op).is_err() {
            return;
        }
    }
    *doc = working;
}

/// The reducer: replays the state log's patches (in its deterministic
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
        apply_patch_body(&mut self.value, event.body.as_str());
    }
}

/// Fold the whole state log into the current derived document.
pub(crate) fn derive_document(state_log: &StateLog) -> Value {
    let mut doc = JsonDoc::default();
    state_log.derive(&mut doc);
    doc.value
}

/// Stable content hash of a derived document, for optimistic concurrency
/// (compare-and-set). `serde_json` serializes object keys in sorted order
/// (`preserve_order` is off in `Cargo.toml`), so the bytes — and thus the hash —
/// are deterministic for a given document regardless of the order its patches
/// folded. `state get` returns this; `state patch --if-doc-hash` compares the
/// caller's value against the current document's, rejecting a stale write.
/// Re-exported at the crate root for embed/SDK consumers doing CAS.
#[must_use]
pub fn document_hash(doc: &Value) -> String {
    let bytes = serde_json::to_vec(doc).unwrap_or_default();
    crate::protocol::identity::content_hash_hex(&bytes)
}

/// Validate a patch at the *write* boundary: it must be in the frozen subset
/// **and** apply cleanly to the current document. No size check here — the
/// op-array is not the signed envelope; `Message::serialize` is the single size
/// gate. (Received patches are not validated; they no-op on failure.)
pub(crate) fn validate_patch(ops: &Value, current: &Value) -> Result<(), String> {
    let frozen = parse_ops(ops).ok_or_else(|| {
        "unsupported patch: only add/replace/remove on object paths and add \"/arr/-\" \
         (no test/move/copy, array indices, or root path)"
            .to_owned()
    })?;
    let mut working = current.clone();
    for op in &frozen {
        apply_one(&mut working, op).map_err(|reject| reject.message(op.path()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{derive_document, patch_body, validate_patch};
    use crate::daemon::state_log::StateLog;
    use crate::protocol::{Message, Nickname, SwarmId};
    use serde_json::{Value, json};

    fn fixture() -> (SwarmId, Nickname) {
        (SwarmId::from("🐝test"), Nickname::from("test-node"))
    }

    /// Build a `State` patch event with a fixed timestamp so replay order is
    /// deterministic in tests.
    fn patch_event(swarm: &SwarmId, author: &Nickname, ts: i64, ops: Value) -> Message {
        let mut msg = Message::new_state(swarm, author, patch_body(ops).unwrap());
        msg.timestamp = ts;
        msg
    }

    #[test]
    fn folds_add_replace_remove_and_array_append() {
        let (swarm, author) = fixture();
        let mut log = StateLog::new();
        log.insert(patch_event(&swarm, &author, 10, json!([{"op":"add","path":"/title","value":"start"}])));
        log.insert(patch_event(&swarm, &author, 20, json!([{"op":"add","path":"/items","value":[]}])));
        log.insert(patch_event(&swarm, &author, 30, json!([{"op":"add","path":"/items/-","value":"first"}])));
        log.insert(patch_event(&swarm, &author, 40, json!([{"op":"replace","path":"/title","value":"updated"}])));
        log.insert(patch_event(&swarm, &author, 50, json!([{"op":"add","path":"/turn","value":"b"},{"op":"add","path":"/scratch","value":1}])));
        log.insert(patch_event(&swarm, &author, 60, json!([{"op":"remove","path":"/scratch"}])));

        assert_eq!(
            derive_document(&log),
            json!({"title":"updated","items":["first"],"turn":"b"})
        );
    }

    #[test]
    fn order_independent_same_set_same_doc() {
        let (swarm, author) = fixture();
        let ops = [
            (30, json!([{"op":"replace","path":"/n","value":3}])),
            (10, json!([{"op":"add","path":"/n","value":1}])),
            (20, json!([{"op":"replace","path":"/n","value":2}])),
        ];
        let mut forward = StateLog::new();
        let mut reverse = StateLog::new();
        for (ts, op) in &ops {
            forward.insert(patch_event(&swarm, &author, *ts, op.clone()));
        }
        for (ts, op) in ops.into_iter().rev() {
            reverse.insert(patch_event(&swarm, &author, ts, op));
        }
        assert_eq!(derive_document(&forward), derive_document(&reverse));
        assert_eq!(derive_document(&forward), json!({"n":3}));
    }

    #[test]
    fn failed_and_out_of_subset_patches_are_no_ops() {
        let (swarm, author) = fixture();
        let mut log = StateLog::new();
        log.insert(patch_event(&swarm, &author, 10, json!([{"op":"add","path":"/a","value":1}])));
        // replace on a missing path — does not apply.
        log.insert(patch_event(&swarm, &author, 20, json!([{"op":"replace","path":"/missing","value":2}])));
        // out-of-subset op (move) — whole patch rejected.
        log.insert(patch_event(&swarm, &author, 30, json!([{"op":"move","from":"/a","path":"/b"}])));
        // root path — rejected.
        log.insert(patch_event(&swarm, &author, 40, json!([{"op":"replace","path":"","value":[]}])));
        // atomicity: a valid op followed by a failing one in the same patch — neither applies.
        log.insert(patch_event(&swarm, &author, 50, json!([{"op":"add","path":"/c","value":3},{"op":"remove","path":"/nope"}])));

        assert_eq!(derive_document(&log), json!({"a":1}));
    }

    #[test]
    fn validate_patch_rejects_out_of_subset_and_non_applying() {
        let current = json!({"a":1});
        assert!(validate_patch(&json!([{"op":"replace","path":"/a","value":2}]), &current).is_ok());
        assert!(validate_patch(&json!([{"op":"add","path":"/b","value":2}]), &current).is_ok());
        // out of subset
        assert!(validate_patch(&json!([{"op":"copy","from":"/a","path":"/b"}]), &current).is_err());
        // does not apply (replace missing)
        assert!(validate_patch(&json!([{"op":"replace","path":"/missing","value":2}]), &current).is_err());
        // root path
        assert!(validate_patch(&json!([{"op":"replace","path":"","value":{}}]), &current).is_err());
    }

    #[test]
    fn array_index_rejection_names_the_cause() {
        // A numeric index into an array is a *permanent* rejection — the message
        // must say so (and name the path), not the generic "does not apply".
        let current = json!({"items": ["a", "b"]});
        let err = validate_patch(
            &json!([{"op":"replace","path":"/items/0","value":"z"}]),
            &current,
        )
        .unwrap_err();
        assert!(err.contains("numeric array indices"), "got: {err}");
        assert!(err.contains("/items/0"), "got: {err}");

        // A missing object key stays the transient "does not apply" wording.
        let missing = validate_patch(
            &json!([{"op":"replace","path":"/nope","value":1}]),
            &current,
        )
        .unwrap_err();
        assert!(missing.contains("does not apply"), "got: {missing}");
    }

    #[test]
    fn document_hash_is_content_addressed_and_key_order_independent() {
        use super::document_hash;
        // Same content, keys written in a different order in the literal — the
        // derived `Value` is identical (Map is sorted, `preserve_order` off), so
        // the hash matches. This is what makes CAS reliable across peers.
        let doc = json!({"turn": "b", "n": 1});
        let reordered = json!({"n": 1, "turn": "b"});
        assert_eq!(document_hash(&doc), document_hash(&reordered));
        // Different content ⇒ different hash.
        assert_ne!(
            document_hash(&doc),
            document_hash(&json!({"turn": "r", "n": 1}))
        );
        assert_eq!(document_hash(&doc).len(), 64, "sha256 hex");
    }
}
