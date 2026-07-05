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
    /// An encrypted channel body (password-protected swarm only): `d` is Base58
    /// of `seal_symmetric(channel_key, plaintext)`, where `plaintext` is the
    /// UTF-8 bytes of a plaintext `change` body — so neither the automerge
    /// change nor the human-readable `m` delta is exposed on the wire. A relay
    /// or a captured frame (no key) sees only opaque bytes; every swarm member
    /// holds the key and decrypts. See [`encrypt_body`]/[`decrypt_body`].
    Enc {
        #[serde(rename = "d")]
        data: String,
    },
}

/// Compose a legacy RFC 7386 merge body. Only the adversarial harness and tests
/// emit this now (the live path uses [`change_body`]); a receiver treats it as a
/// no-op.
///
/// # Errors
/// Serialization failure or a body exceeding the size limit.
#[cfg(any(test, feature = "adversarial"))]
pub fn merge_body(merge: Value) -> anyhow::Result<MessageBody> {
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
pub fn change_body(change: &[u8], merge: Option<&Value>) -> anyhow::Result<MessageBody> {
    let json = serde_json::to_string(&StateOp::Change {
        data: bs58::encode(change).into_string(),
        merge: merge.cloned(),
    })?;
    MessageBody::new(json).map_err(|error| anyhow::anyhow!("{error}"))
}

/// Decode a channel-event body back to raw automerge change bytes. `None` for a
/// non-`change` body (a legacy `merge`, an `enc` envelope, or anything that
/// doesn't parse) — such a body is a no-op on the automerge doc. Call
/// [`decrypt_body`] first for a passworded swarm's `enc` bodies.
pub(crate) fn parse_change_body(body: &str) -> Option<Vec<u8>> {
    match serde_json::from_str::<StateOp>(body).ok()? {
        StateOp::Change { data, .. } => bs58::decode(&data).into_vec().ok(),
        StateOp::Merge { .. } | StateOp::Enc { .. } => None,
    }
}

/// Encrypt a plaintext body under `key`, producing an `enc` envelope. Seals the
/// whole plaintext body — used for the `state`/`meta` change bodies (both the
/// automerge change and the `m` delta) and for broadcast chat (`A2aMsg`) bodies.
///
/// # Errors
/// Serialization or `MessageBody` validation of the envelope fails.
pub fn encrypt_body(plain: &MessageBody, key: &[u8; 32]) -> anyhow::Result<MessageBody> {
    let blob = crate::protocol::seal::seal_symmetric(key, plain.as_str().as_bytes());
    let json = serde_json::to_string(&StateOp::Enc {
        data: bs58::encode(blob).into_string(),
    })?;
    MessageBody::new(json).map_err(|error| anyhow::anyhow!("{error}"))
}

/// Recover the plaintext body string. With a `key` (passworded swarm) the body
/// **must** be a sealed `enc` envelope: it is opened, and any non-`enc` body is
/// rejected (`None`) so cleartext can never slip past the encryption guarantee.
/// Without a key (passwordless) a body is returned unchanged. `None` also when
/// an `enc` body can't be opened (wrong key / malformed) — the caller treats a
/// `None` as an opaque no-op (state/meta) or a drop (chat).
pub(crate) fn decrypt_body(body: &str, key: Option<&[u8; 32]>) -> Option<String> {
    match serde_json::from_str::<StateOp>(body) {
        Ok(StateOp::Enc { data }) => {
            let key = key?;
            let blob = bs58::decode(&data).into_vec().ok()?;
            let plain = crate::protocol::seal::open_symmetric(key, &blob).ok()?;
            String::from_utf8(plain).ok()
        }
        // A non-`enc` body: passthrough on a passwordless swarm (no key), but
        // rejected on a passworded one — encryption is enforced, not optional.
        _ => match key {
            Some(_) => None,
            None => Some(body.to_owned()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{change_body, decrypt_body, encrypt_body, merge_body, parse_change_body};

    #[test]
    fn enc_body_round_trips_and_hides_the_delta() {
        let key = [3u8; 32];
        let plain =
            change_body(&[1, 2, 3, 4], Some(&serde_json::json!({"board": "x"}))).expect("compose");
        let enc = encrypt_body(&plain, &key).expect("encrypt");
        assert_ne!(
            enc.as_str(),
            plain.as_str(),
            "wire body is not the plaintext"
        );
        assert!(
            !enc.as_str().contains("board"),
            "the merge delta must not leak into the ciphertext body"
        );
        // decrypt_body recovers the exact plaintext body, which then parses.
        let recovered = decrypt_body(enc.as_str(), Some(&key)).expect("decrypt");
        assert_eq!(recovered, plain.as_str());
        assert_eq!(parse_change_body(&recovered), Some(vec![1u8, 2, 3, 4]));
    }

    #[test]
    fn enc_body_needs_the_right_key() {
        let enc = encrypt_body(&change_body(&[9, 9], None).unwrap(), &[1u8; 32]).unwrap();
        assert!(
            decrypt_body(enc.as_str(), Some(&[2u8; 32])).is_none(),
            "wrong key"
        );
        assert!(decrypt_body(enc.as_str(), None).is_none(), "no key");
        // An `enc` body is not itself a change (must be decrypted first).
        assert_eq!(parse_change_body(enc.as_str()), None);
    }

    #[test]
    fn decrypt_enforces_encryption_only_when_keyed() {
        let plain = change_body(&[5], None).expect("compose");
        // Passwordless (no key): a non-`enc` body is returned verbatim.
        assert_eq!(
            decrypt_body(plain.as_str(), None).as_deref(),
            Some(plain.as_str())
        );
        // Passworded (key set): a non-`enc` body is rejected — cleartext can't
        // slip past the encryption guarantee.
        assert!(decrypt_body(plain.as_str(), Some(&[7u8; 32])).is_none());
    }

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
