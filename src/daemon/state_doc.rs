//! The `state`/`meta` channel wire body: the tagged envelope that carries one
//! automerge change (or, from an old binary, a legacy RFC 7386 merge the
//! automerge engine treats as a no-op). The convergent document itself — merge,
//! authorization, and reconciliation — lives in [`super::doc`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::protocol::message::MessageBody;

/// The tagged `State`/`Meta` message body.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
enum StateOp {
    /// The live wire form: one automerge change, Base58-encoded so the body stays
    /// `MessageBody`-safe (wire key `c`), plus an optional `m` carrying the
    /// RFC 7386 merge the author applied — the human-readable delta the
    /// `state`/`meta` output event surfaces (the change bytes themselves are
    /// opaque). Omitted for internal writes that are never surfaced (the card
    /// publish), so those stay lean.
    Change {
        #[serde(rename = "c")]
        data: String,
        #[serde(rename = "m", default, skip_serializing_if = "Option::is_none")]
        merge: Option<Value>,
    },
    /// The pre-automerge RFC 7386 merge shape. Retained only so a body still
    /// composes for the adversarial harness; the automerge engine ignores it (a
    /// no-op), which is the forward-compat behavior an old binary now hits.
    Merge { merge: Value },
}

/// Compose a legacy RFC 7386 merge body. Only the adversarial harness and tests
/// emit this now (the live path uses [`change_body`]); a receiver treats it as a
/// no-op.
///
/// # Errors
/// Serialization failure or a body exceeding the size limit.
#[cfg(any(test, feature = "adversarial"))]
pub(crate) fn merge_body(merge: Value) -> anyhow::Result<MessageBody> {
    let json = serde_json::to_string(&StateOp::Merge { merge })?;
    MessageBody::new(json).map_err(|error| anyhow::anyhow!("{error}"))
}

/// Wrap one automerge change's raw bytes as a channel-event body. `merge` is the
/// RFC 7386 delta the author applied, carried for the output event's
/// human-readable `merge` field; pass `None` for an internal write that is never
/// surfaced (keeps the body lean — no delta duplicated alongside the change).
///
/// # Errors
/// Serialization failure or a body exceeding the size limit.
pub(crate) fn change_body(change: &[u8], merge: Option<&Value>) -> anyhow::Result<MessageBody> {
    let json = serde_json::to_string(&StateOp::Change {
        data: bs58::encode(change).into_string(),
        merge: merge.cloned(),
    })?;
    MessageBody::new(json).map_err(|error| anyhow::anyhow!("{error}"))
}

/// Decode a channel-event body back to raw automerge change bytes. `None` for a
/// non-`change` body (a legacy `merge`, or anything that doesn't parse) — such a
/// body is a no-op on the automerge doc.
pub(crate) fn parse_change_body(body: &str) -> Option<Vec<u8>> {
    match serde_json::from_str::<StateOp>(body).ok()? {
        StateOp::Change { data, .. } => bs58::decode(&data).into_vec().ok(),
        StateOp::Merge { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{change_body, merge_body, parse_change_body};

    #[test]
    fn change_body_round_trips() {
        let change = vec![1u8, 2, 3, 250, 0, 42];
        // The optional `m` delta does not affect change decoding either way.
        let with_merge =
            change_body(&change, Some(&serde_json::json!({"k": "v"}))).expect("compose");
        assert_eq!(parse_change_body(with_merge.as_str()), Some(change.clone()));
        let lean = change_body(&change, None).expect("compose");
        assert_eq!(parse_change_body(lean.as_str()), Some(change));
    }

    #[test]
    fn legacy_merge_body_is_a_no_op_change() {
        // A legacy `merge` body (or any non-`change` body) decodes to no change,
        // so the automerge engine ignores it — forward-compat with old binaries.
        let body = merge_body(serde_json::json!({"turn": "a"})).expect("compose");
        assert_eq!(parse_change_body(body.as_str()), None);
        assert_eq!(parse_change_body("not json"), None);
    }
}
