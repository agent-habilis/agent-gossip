//! Per-participant **identity**: the Ed25519 keypair that authenticates
//! every message this member authors.
//!
//! Distinct from the two other keys in the system: the transport
//! `EndpointId` (authenticates the *connection*) and the seed-derived
//! rendezvous key (shared by every member — see [`super::crypto`]). This
//! is the first **per-author** credential, and the root of
//! [`docs/history-integrity.md`].
//!
//! The public key is the durable identity; the [`Nickname`] is a display
//! label bound to it trust-on-first-use by the receiver.
//!
//! The key is currently **in-process / ephemeral** — minted once per
//! `create`/`join` and held in memory for the process lifetime. A process
//! restart therefore mints a *new* key, so under the same nickname it is
//! seen as a new identity (peers that pinned the old key flag it). Stable
//! cross-restart identity (on-disk persistence) is a documented follow-up;
//! see `docs/history-integrity.md`.

use anyhow::{Context, Result};
use iroh::{PublicKey, SecretKey, Signature};
use rand::RngCore;
use sha2::{Digest, Sha256};

/// A per-participant signing identity — just the secret key. The nickname
/// it speaks under is carried by the message `author` field and bound to
/// this key trust-on-first-use by receivers, so it is not stored here.
pub(crate) struct Identity {
    secret: SecretKey,
}

impl Identity {
    /// Mint a fresh random identity. `SecretKey::from_bytes` is infallible
    /// — any 32 bytes is a valid Ed25519 secret.
    #[must_use]
    pub(crate) fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        Identity {
            secret: SecretKey::from_bytes(&bytes),
        }
    }

    #[must_use]
    pub(crate) fn public(&self) -> PublicKey {
        self.secret.public()
    }

    /// Sign `bytes` (the message's canonical bytes) with our secret.
    #[must_use]
    pub(crate) fn sign(&self, bytes: &[u8]) -> Signature {
        self.secret.sign(bytes)
    }
}

/// Verify `signature` over `bytes` against `pubkey`. A malformed or
/// non-matching signature returns `false` (never panics).
#[must_use]
pub(crate) fn verify(pubkey: &PublicKey, bytes: &[u8], signature: &Signature) -> bool {
    pubkey.verify(bytes, signature).is_ok()
}

/// SHA-256 content hash of `bytes`, lowercase hex — the self-certifying
/// message id used by the per-author backlink (`prev`) and (Phase 3) the
/// cross-author DAG. Computed from a message's canonical bytes, so it is
/// recomputable by any receiver and never has to be trusted off the wire.
#[must_use]
pub(crate) fn content_hash_hex(bytes: &[u8]) -> String {
    to_hex(&Sha256::digest(bytes))
}

// ── wire encoding ──────────────────────────────────────────────────────
//
// Public keys (32 bytes) and signatures (64 bytes) travel as lowercase
// hex strings in the JSON envelope, so the `--output json` wire stays
// inspectable and does not depend on iroh's serde representation.

#[must_use]
pub(crate) fn encode_pubkey(pubkey: &PublicKey) -> String {
    to_hex(pubkey.as_bytes())
}

/// Parse a hex public key from the wire. Errors on bad hex or length, or
/// a point that is not a valid Ed25519 key.
pub(crate) fn decode_pubkey(hex: &str) -> Result<PublicKey> {
    let bytes: [u8; 32] = from_hex(hex)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("public key must be 32 bytes"))?;
    PublicKey::from_bytes(&bytes).context("invalid Ed25519 public key")
}

#[must_use]
pub(crate) fn encode_sig(signature: &Signature) -> String {
    to_hex(&signature.to_bytes())
}

/// Parse a hex Ed25519 signature (64 bytes) from the wire.
pub(crate) fn decode_sig(hex: &str) -> Result<Signature> {
    let bytes: [u8; 64] = from_hex(hex)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("signature must be 64 bytes"))?;
    Ok(Signature::from_bytes(&bytes))
}

fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit(u32::from(byte >> 4), 16).expect("nibble < 16"));
        out.push(char::from_digit(u32::from(byte & 0x0f), 16).expect("nibble < 16"));
    }
    out
}

fn from_hex(hex: &str) -> Result<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        anyhow::bail!("hex string has odd length");
    }
    (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).context("invalid hex digit"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Identity, decode_pubkey, decode_sig, encode_pubkey, encode_sig, verify};

    #[test]
    fn sign_verify_round_trip() {
        let identity = Identity::generate();
        let bytes = b"canonical message bytes";
        let sig = identity.sign(bytes);
        assert!(verify(&identity.public(), bytes, &sig));
    }

    #[test]
    fn tampered_bytes_fail_verification() {
        let identity = Identity::generate();
        let sig = identity.sign(b"original");
        assert!(!verify(&identity.public(), b"tampered", &sig));
    }

    #[test]
    fn wrong_key_fails_verification() {
        let signer = Identity::generate();
        let other = Identity::generate();
        let bytes = b"hello";
        let sig = signer.sign(bytes);
        assert!(!verify(&other.public(), bytes, &sig));
    }

    #[test]
    fn pubkey_hex_round_trip() {
        let identity = Identity::generate();
        let hex = encode_pubkey(&identity.public());
        assert_eq!(hex.len(), 64);
        assert_eq!(decode_pubkey(&hex).unwrap(), identity.public());
    }

    #[test]
    fn sig_hex_round_trip() {
        let identity = Identity::generate();
        let sig = identity.sign(b"x");
        let hex = encode_sig(&sig);
        assert_eq!(hex.len(), 128);
        assert!(verify(&identity.public(), b"x", &decode_sig(&hex).unwrap()));
    }

    #[test]
    fn decode_rejects_bad_hex() {
        assert!(decode_pubkey("zz").is_err());
        assert!(decode_pubkey("abc").is_err()); // odd length
        assert!(decode_sig("00").is_err()); // wrong length
    }
}
