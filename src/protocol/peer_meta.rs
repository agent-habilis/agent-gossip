//! Per-participant **metadata** the agent self-reports at `create`/`join`:
//! the model and harness it runs on (e.g. `Opus 4.8` / `Claude Code`). The
//! daemon can't know these — the agent driving it supplies them — so they
//! travel in the `joined` presence body, signed by the author. Membership
//! metadata, distinct from the `PeerInfo` address codec (`peer_addr`): kept
//! in its own module so the two never get conflated.

use serde::{Deserialize, Serialize};

use crate::protocol::message::MessageBody;

/// Cap on each value's length (chars), so one peer can't bloat every other
/// peer's roster with an oversized self-description.
const VALUE_MAX_CHARS: usize = 64;

/// A participant's self-reported model + harness. Either may be absent (a
/// node started without `--model`/`--harness`). Authenticated by the sender's
/// signature, but otherwise trust-on-use — it is display metadata, not a
/// claim the daemon verifies.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PeerMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
}

impl PeerMeta {
    /// Build from the local node's `--model`/`--harness`, sanitizing each so
    /// the resulting `joined` body is always a valid [`MessageBody`].
    pub(crate) fn from_refs(model: Option<&str>, harness: Option<&str>) -> Self {
        Self {
            model: model.map(sanitize),
            harness: harness.map(sanitize),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.model.is_none() && self.harness.is_none()
    }

    /// A one-line `Model / Harness` label for display (just the present part
    /// when only one is set), or `None` when neither is.
    pub(crate) fn label(&self) -> Option<String> {
        match (self.model.as_deref(), self.harness.as_deref()) {
            (Some(model), Some(harness)) => Some(format!("{model} / {harness}")),
            (Some(only), None) | (None, Some(only)) => Some(only.to_owned()),
            (None, None) => None,
        }
    }
}

/// Strip control characters and clamp to [`VALUE_MAX_CHARS`] so the value is
/// safe to embed in a signed body and to render in a terminal table.
fn sanitize(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_control())
        .take(VALUE_MAX_CHARS)
        .collect()
}

/// Serialize to a `joined` message body: a compact JSON object with the
/// present keys, or an empty body when nothing is set.
pub(crate) fn to_body(meta: &PeerMeta) -> MessageBody {
    if meta.is_empty() {
        return MessageBody::new("").expect("empty string is a valid MessageBody");
    }
    let json = serde_json::to_string(meta).expect("PeerMeta always serializes");
    MessageBody::new(json).expect("sanitized PeerMeta has no control characters")
}

/// Parse a `joined` body back into [`PeerMeta`]. Empty or unparseable bodies
/// (e.g. an older `joined` with no metadata) yield an empty `PeerMeta`.
pub(crate) fn from_body(body: &str) -> PeerMeta {
    if body.is_empty() {
        return PeerMeta::default();
    }
    serde_json::from_str(body).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{PeerMeta, from_body, to_body};

    #[test]
    fn round_trips_both_fields() {
        let meta = PeerMeta::from_refs(Some("Opus 4.8"), Some("Claude Code"));
        let parsed = from_body(to_body(&meta).as_str());
        assert_eq!(parsed, meta);
        assert_eq!(parsed.model.as_deref(), Some("Opus 4.8"));
        assert_eq!(parsed.harness.as_deref(), Some("Claude Code"));
    }

    #[test]
    fn round_trips_partial() {
        let meta = PeerMeta::from_refs(Some("Opus 4.8"), None);
        let parsed = from_body(to_body(&meta).as_str());
        assert_eq!(parsed.model.as_deref(), Some("Opus 4.8"));
        assert_eq!(parsed.harness, None);
    }

    #[test]
    fn empty_meta_is_empty_body() {
        let meta = PeerMeta::default();
        assert!(meta.is_empty());
        assert_eq!(to_body(&meta).as_str(), "");
        assert_eq!(from_body(""), PeerMeta::default());
    }

    #[test]
    fn sanitizes_control_chars_and_length() {
        let long = "a".repeat(100);
        let meta = PeerMeta::from_refs(Some("op\u{7}us"), Some(&long));
        // Control char dropped, value clamped to the cap.
        assert_eq!(meta.model.as_deref(), Some("opus"));
        assert_eq!(meta.harness.as_deref().map(str::len), Some(64));
        // Still a valid, parseable body.
        assert_eq!(from_body(to_body(&meta).as_str()), meta);
    }
}
