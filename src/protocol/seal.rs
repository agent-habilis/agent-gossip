//! End-to-end sealing of a directed frame body: a NaCl-sealed-box-style
//! construction so only the addressed recipient can read it. A relay still
//! forwards the frame and verifies its Ed25519 signature (which covers the
//! ciphertext), but cannot open it.
//!
//! Construction: a fresh **ephemeral** X25519 keypair per frame → ECDH with the
//! recipient's static X25519 public key → the shared secret is run through the
//! in-tree [`derive_secret`] KDF (SHA-256, domain-separated) into a 32-byte AEAD
//! key → ChaCha20-Poly1305. The ephemeral public key travels in the envelope, so
//! the recipient (holding the static secret) can re-derive the shared secret;
//! nobody else can. The ephemeral key gives forward secrecy; **sender
//! authenticity comes from the frame's Ed25519 signature**, not the box.
//!
//! The envelope is a small JSON object with Base58 fields, so it is a valid
//! [`MessageBody`] (UTF-8, no control characters).

use anyhow::{Context, Result, anyhow};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use x25519_dalek::{PublicKey, StaticSecret};

use super::MessageBody;
use super::crypto::derive_secret;

/// KDF label separating the seal key from every other seed-derived secret, and
/// the AEAD associated data (bound into the tag).
const SEAL_LABEL: &[u8] = b"agent-gossip/seal/v1";

/// AEAD associated data (construction tag) for the symmetric channel seal. The
/// key is already domain-separated by the caller (a `derive_secret` channel
/// label), so this only pins the construction version.
const SYM_AAD: &[u8] = b"agent-gossip/sym/v1";

/// Envelope construction tag — bumped only on a breaking crypto change.
const ENVELOPE_VERSION: &str = "x25519-chacha20poly1305/1";

/// A sealed payload: the ephemeral X25519 public key, the AEAD nonce, and the
/// ciphertext (with the 16-byte Poly1305 tag appended by the AEAD).
struct Sealed {
    epk: [u8; 32],
    nonce: [u8; 12],
    ct: Vec<u8>,
}

/// The on-wire JSON form of a [`Sealed`] — Base58 fields keep it MessageBody-safe.
#[derive(Serialize, Deserialize)]
struct Envelope {
    #[serde(rename = "v")]
    version: String,
    epk: String,
    #[serde(rename = "n")]
    nonce: String,
    ct: String,
}

/// Seal `plaintext` to `recipient_pub` (a raw X25519 public key). Infallible in
/// practice — the only fallible step is AEAD encryption, which cannot fail for a
/// well-formed key/nonce.
fn seal(recipient_pub: &[u8; 32], plaintext: &[u8]) -> Sealed {
    let mut eph_bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut eph_bytes);
    let ephemeral = StaticSecret::from(eph_bytes);
    let epk = PublicKey::from(&ephemeral);
    let shared = ephemeral.diffie_hellman(&PublicKey::from(*recipient_pub));

    let key = derive_secret(shared.as_bytes(), SEAL_LABEL);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    let mut nonce = [0u8; 12];
    rand::rng().fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: SEAL_LABEL,
            },
        )
        .expect("ChaCha20-Poly1305 encryption cannot fail for a valid key/nonce");
    Sealed {
        epk: *epk.as_bytes(),
        nonce,
        ct,
    }
}

/// Open a [`Sealed`] with our static X25519 secret.
///
/// # Errors
/// The AEAD fails to authenticate — a wrong recipient key or a tampered
/// ciphertext/nonce.
fn unseal(our_secret: &StaticSecret, sealed: &Sealed) -> Result<Vec<u8>> {
    let shared = our_secret.diffie_hellman(&PublicKey::from(sealed.epk));
    let key = derive_secret(shared.as_bytes(), SEAL_LABEL);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    cipher
        .decrypt(
            Nonce::from_slice(&sealed.nonce),
            Payload {
                msg: &sealed.ct,
                aad: SEAL_LABEL,
            },
        )
        .map_err(|_| anyhow!("unseal failed: wrong recipient key or tampered ciphertext"))
}

/// Seal `plaintext` (a serialized A2A payload) to `recipient_pub`, returning the
/// envelope as a [`MessageBody`] ready to drop into a directed frame.
///
/// # Errors
/// The envelope (pure ASCII JSON) somehow fails `MessageBody` validation.
pub(crate) fn seal_to_body(recipient_pub: &[u8; 32], plaintext: &str) -> Result<MessageBody> {
    let sealed = seal(recipient_pub, plaintext.as_bytes());
    let envelope = Envelope {
        version: ENVELOPE_VERSION.to_string(),
        epk: bs58::encode(sealed.epk).into_string(),
        nonce: bs58::encode(sealed.nonce).into_string(),
        ct: bs58::encode(sealed.ct).into_string(),
    };
    let json = serde_json::to_string(&envelope).expect("Envelope always serializes");
    MessageBody::new(json).map_err(|error| anyhow!("{error}"))
}

/// Open a sealed directed `body` with our static X25519 secret, recovering the
/// plaintext A2A payload string.
///
/// # Errors
/// The body is not a sealed envelope, has an unknown version, carries malformed
/// Base58, or fails to decrypt/authenticate.
pub(crate) fn unseal_from_body(our_secret: &StaticSecret, body: &MessageBody) -> Result<String> {
    let envelope: Envelope =
        serde_json::from_str(body.as_str()).context("body is not a sealed envelope")?;
    if envelope.version != ENVELOPE_VERSION {
        return Err(anyhow!(
            "unsupported seal envelope version: {}",
            envelope.version
        ));
    }
    let epk = decode_array::<32>(&envelope.epk).context("bad ephemeral key")?;
    let nonce = decode_array::<12>(&envelope.nonce).context("bad nonce")?;
    let ct = bs58::decode(&envelope.ct)
        .into_vec()
        .context("bad ciphertext")?;
    let plaintext = unseal(our_secret, &Sealed { epk, nonce, ct })?;
    String::from_utf8(plaintext).context("decrypted payload is not valid UTF-8")
}

fn decode_array<const N: usize>(encoded: &str) -> Result<[u8; N]> {
    let bytes = bs58::decode(encoded).into_vec().context("invalid Base58")?;
    bytes.try_into().map_err(|_| anyhow!("expected {N} bytes"))
}

/// Symmetrically seal `plaintext` under a pre-shared 32-byte `key` (a
/// swarm-password-derived channel key). Layout: `nonce(12) ‖ ct+tag`. Unlike
/// [`seal_to_body`] there is no ECDH — every holder of `key` can open it, which
/// is exactly the shared-channel (state/meta) model: membership already gated
/// the password, and this keeps the payload unreadable to a relay or a captured
/// frame.
pub(crate) fn seal_symmetric(key: &[u8; 32], plaintext: &[u8]) -> Vec<u8> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let mut nonce = [0u8; 12];
    rand::rng().fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: SYM_AAD,
            },
        )
        .expect("ChaCha20-Poly1305 encryption cannot fail for a valid key/nonce");
    let mut out = Vec::with_capacity(nonce.len() + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

/// Open a [`seal_symmetric`] blob with the same `key`.
///
/// # Errors
/// The blob is too short to carry a nonce, or the AEAD fails to authenticate (a
/// wrong key or tampered bytes).
pub(crate) fn open_symmetric(key: &[u8; 32], blob: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < 12 {
        return Err(anyhow!("symmetric blob too short for a nonce"));
    }
    let (nonce, ct) = blob.split_at(12);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ct,
                aad: SYM_AAD,
            },
        )
        .map_err(|_| anyhow!("symmetric open failed: wrong key or tampered ciphertext"))
}

#[cfg(test)]
mod tests {
    use super::{open_symmetric, seal_symmetric, seal_to_body, unseal_from_body};
    use x25519_dalek::{PublicKey, StaticSecret};

    #[test]
    fn symmetric_round_trips() {
        let key = [7u8; 32];
        for plaintext in [b"".as_slice(), b"hello state doc", &[0u8; 5000]] {
            let blob = seal_symmetric(&key, plaintext);
            assert_ne!(
                blob.as_slice(),
                plaintext,
                "ciphertext is not the plaintext"
            );
            assert_eq!(open_symmetric(&key, &blob).unwrap(), plaintext);
        }
    }

    #[test]
    fn symmetric_wrong_key_or_tamper_fails() {
        let blob = seal_symmetric(&[1u8; 32], b"the shared state");
        assert!(
            open_symmetric(&[2u8; 32], &blob).is_err(),
            "a wrong key must not open the blob"
        );
        let mut tampered = blob.clone();
        *tampered.last_mut().unwrap() ^= 0x01;
        assert!(
            open_symmetric(&[1u8; 32], &tampered).is_err(),
            "a flipped byte must fail the AEAD tag"
        );
        assert!(
            open_symmetric(&[1u8; 32], &[0u8; 4]).is_err(),
            "a blob too short for a nonce must fail cleanly"
        );
    }

    fn keypair(byte: u8) -> (StaticSecret, [u8; 32]) {
        let secret = StaticSecret::from([byte; 32]);
        let public = *PublicKey::from(&secret).as_bytes();
        (secret, public)
    }

    #[test]
    fn round_trips_to_the_recipient() {
        let (secret, public) = keypair(1);
        let plaintext = r#"{"kind":"status-update","taskId":"7f3e"}"#;
        let body = seal_to_body(&public, plaintext).unwrap();
        assert_eq!(unseal_from_body(&secret, &body).unwrap(), plaintext);
    }

    #[test]
    fn a_wrong_recipient_cannot_open_it() {
        let (_recipient, recipient_pub) = keypair(1);
        let (eavesdropper, _) = keypair(2);
        let body = seal_to_body(&recipient_pub, "secret").unwrap();
        assert!(
            unseal_from_body(&eavesdropper, &body).is_err(),
            "a non-recipient must not be able to open the envelope"
        );
    }

    #[test]
    fn a_tampered_ciphertext_fails() {
        let (secret, public) = keypair(3);
        let body = seal_to_body(&public, "the quick brown fox").unwrap();
        // Flip the last Base58 char of the `ct` field.
        let mut json: serde_json::Value = serde_json::from_str(body.as_str()).unwrap();
        let ct = json["ct"].as_str().unwrap().to_owned();
        let mut chars: Vec<char> = ct.chars().collect();
        let last = chars.last_mut().unwrap();
        *last = if *last == '1' { '2' } else { '1' };
        json["ct"] = serde_json::Value::String(chars.into_iter().collect());
        let tampered = crate::protocol::MessageBody::new(json.to_string()).unwrap();
        assert!(
            unseal_from_body(&secret, &tampered).is_err(),
            "a tampered ciphertext must fail the AEAD tag"
        );
    }

    #[test]
    fn seals_empty_and_large_payloads() {
        let (secret, public) = keypair(4);
        for plaintext in ["", &"x".repeat(40_000)] {
            let body = seal_to_body(&public, plaintext).unwrap();
            assert_eq!(unseal_from_body(&secret, &body).unwrap(), plaintext);
        }
    }

    #[test]
    fn a_plaintext_body_does_not_open_as_sealed() {
        let (secret, _) = keypair(5);
        let body = crate::protocol::MessageBody::new(r#"{"kind":"message"}"#.to_owned()).unwrap();
        assert!(
            unseal_from_body(&secret, &body).is_err(),
            "an un-sealed body must not be mistaken for an envelope"
        );
    }
}
