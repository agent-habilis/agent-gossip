//! The blob ticket — a `🎟️` token carrying everything a consumer needs to fetch
//! one content-addressed blob from its producer: the bearer secret, the content
//! hash + size, the mesh's discovery config, and the producer's blob-endpoint
//! address. Payload layout: `secret(32) ‖ flags(1) ‖ sha256(32) ‖ size(8, LE) ‖
//! lookups ‖ address-json` (lookups is self-delimiting, so the address occupies
//! the remainder). Bit 0 of the flags byte marks a password-protected ticket.
//!
//! Wire: the `🎟️` ticket brand + Base58Check(`version ‖ kind ‖ payload`) with a
//! `SHA256d` checksum — the emoji is the brand, the remainder after `://` ASCII
//! Base58. The `🎟️` glyph is distinct from the mesh id's `💬`, so a mesh id
//! fails ticket decode on the prefix alone. The brand *is* shared with the a2a
//! bridge ticket, so the `kind` byte marks this as a *blob* ticket and makes a
//! wrong-kind token (an a2a ticket) fail cleanly on decode. Mirrors the a2a
//! bridge ticket.

use anyhow::{Context, Result, bail};
use iroh::EndpointAddr;
use sha2::{Digest, Sha256};

use crate::protocol::peer_addr::{endpoint_addr_from_json, endpoint_addr_to_json};
use crate::protocol::mesh::LookupOpts;
use crate::util::consts::{MESH_URI_SEPARATOR, TICKET_GLYPH};

use super::{HASH_LEN, SECRET_LEN};

/// Branding prefix on every ticket — the `🎟️` ticket glyph; the remainder of the
/// string is ASCII Base58Check. Blob and a2a tickets share this brand and are
/// told apart by [`KIND`]; the mesh id's `💬` is a different glyph entirely.
pub(crate) const PREFIX: &str = TICKET_GLYPH;

/// Framing version. Bumped only on a breaking framing change; an unknown
/// version is rejected on decode.
const VERSION: u8 = 1;

/// Ticket-kind discriminant, framed after [`VERSION`]. Distinct from the a2a
/// bridge ticket's kind so a token of the wrong kind is rejected on decode now
/// that both share the `🎟️` brand.
const KIND: u8 = 1;

/// Bit 0 of the flags byte: the password flag.
const PASSWORD_BIT: u8 = 0b0000_0001;

/// A decoded blob ticket.
#[derive(Debug)]
pub struct BlobTicket {
    pub addr: EndpointAddr,
    pub secret: [u8; SECRET_LEN],
    pub sha256: [u8; HASH_LEN],
    pub size: u64,
    pub lookups: LookupOpts,
    /// Password-protected: the consumer must present the Argon2id stretch of the
    /// password (salted by `secret`) in the stream header instead of the raw
    /// secret, so the ticket no longer redeems alone.
    pub password: bool,
}

impl BlobTicket {
    /// Encode as a `🎟️` blob token.
    #[must_use]
    /// # Panics
    /// Panics if an internal invariant is violated.
    pub fn encode(&self) -> String {
        let mut payload = Vec::with_capacity(SECRET_LEN + 1 + HASH_LEN + 8 + 64);
        payload.extend_from_slice(&self.secret);
        payload.push(if self.password { PASSWORD_BIT } else { 0 });
        payload.extend_from_slice(&self.sha256);
        payload.extend_from_slice(&self.size.to_le_bytes());
        self.lookups.encode_into(&mut payload);
        let addr_json = serde_json::to_vec(&endpoint_addr_to_json(&self.addr))
            .expect("EndpointAddr JSON always serializes");
        payload.extend_from_slice(&addr_json);
        let mut framed = Vec::with_capacity(2 + payload.len());
        framed.push(VERSION);
        framed.push(KIND);
        framed.extend_from_slice(&payload);
        format!("{PREFIX}{MESH_URI_SEPARATOR}{}", base58check_encode(&framed))
    }

    /// The blob's content hash as lowercase hex — the content-addressed name to
    /// land the fetched bytes under, mirroring the producer's spool filename
    /// (`src/blob/store.rs`).
    #[must_use]
    pub fn sha256_hex(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::with_capacity(self.sha256.len() * 2);
        for byte in &self.sha256 {
            let _ = write!(out, "{byte:02x}");
        }
        out
    }

    /// Decode a `🎟️` blob ticket.
    ///
    /// # Errors
    /// Not a `🎟️` token, the wrong ticket kind, a bad checksum/version, or a
    /// malformed payload.
    pub fn decode(ticket: &str) -> Result<Self> {
        let body = strip_ticket_prefix(ticket.trim())
            .with_context(|| format!("not a blob ticket: must start with {PREFIX}"))?;
        let framed = base58check_decode(body)?;
        let version = *framed.first().context("ticket too short")?;
        if version != VERSION {
            bail!("unsupported blob ticket version: {version}");
        }
        let kind = *framed.get(1).context("ticket too short")?;
        if kind != KIND {
            bail!("not a blob ticket: wrong ticket kind");
        }
        let payload = &framed[2..];
        let mut pos = 0;
        let secret =
            take_array::<SECRET_LEN>(payload, &mut pos).context("ticket missing secret")?;
        let flags = *payload.get(pos).context("ticket missing flags")?;
        pos += 1;
        let password = flags & PASSWORD_BIT != 0;
        let sha256 = take_array::<HASH_LEN>(payload, &mut pos).context("ticket missing hash")?;
        let size_bytes = take_array::<8>(payload, &mut pos).context("ticket missing size")?;
        let size = u64::from_le_bytes(size_bytes);
        let lookups = LookupOpts::decode_from(payload, &mut pos)?;
        let addr_json = payload.get(pos..).context("ticket missing address")?;
        let value: serde_json::Value =
            serde_json::from_slice(addr_json).context("invalid ticket address json")?;
        let (_id, addr) = endpoint_addr_from_json(&value)?;
        Ok(Self {
            addr,
            secret,
            sha256,
            size,
            lookups,
            password,
        })
    }
}

/// Read `N` bytes at `*pos` into a fixed array, advancing `*pos`. `None` if the
/// slice is too short.
fn take_array<const N: usize>(bytes: &[u8], pos: &mut usize) -> Option<[u8; N]> {
    let slice = bytes.get(*pos..*pos + N)?;
    let mut out = [0u8; N];
    out.copy_from_slice(slice);
    *pos += N;
    Some(out)
}

/// Strip the ticket brand and optional `://` off a token, returning the
/// Base58Check body. Accepts the canonical `🎟️://` and, defensively, a paste
/// that dropped the VS-16 (`🎟://`) or the separator. Mirrors the a2a bridge
/// ticket. `None` if the token doesn't carry the ticket glyph.
fn strip_ticket_prefix(token: &str) -> Option<&str> {
    let base = TICKET_GLYPH.strip_suffix('\u{FE0F}').unwrap_or(TICKET_GLYPH);
    let rest = token.strip_prefix(base)?;
    let rest = rest.strip_prefix('\u{FE0F}').unwrap_or(rest);
    Some(rest.strip_prefix(MESH_URI_SEPARATOR).unwrap_or(rest))
}

fn checksum(bytes: &[u8]) -> [u8; 4] {
    let first = Sha256::digest(bytes);
    let second = Sha256::digest(first);
    let mut out = [0u8; 4];
    out.copy_from_slice(&second[..4]);
    out
}

fn base58check_encode(payload: &[u8]) -> String {
    let mut with_checksum = payload.to_vec();
    with_checksum.extend_from_slice(&checksum(payload));
    bs58::encode(with_checksum).into_string()
}

fn base58check_decode(encoded: &str) -> Result<Vec<u8>> {
    let decoded = bs58::decode(encoded)
        .into_vec()
        .context("invalid Base58 in blob ticket")?;
    if decoded.len() < 4 {
        bail!("blob ticket too short");
    }
    let (payload, received) = decoded.split_at(decoded.len() - 4);
    if received != checksum(payload) {
        bail!("invalid blob ticket checksum");
    }
    Ok(payload.to_vec())
}

#[cfg(test)]
mod tests {
    use super::{BlobTicket, PREFIX};
    use crate::blob::{HASH_LEN, SECRET_LEN};
    use crate::protocol::mesh::LookupOpts;
    use iroh::{EndpointAddr, SecretKey};

    fn sample_addr(byte: u8) -> EndpointAddr {
        let id = SecretKey::from_bytes(&[byte; 32]).public();
        EndpointAddr::new(id).with_ip_addr("127.0.0.1:4242".parse().unwrap())
    }

    fn sample(password: bool) -> BlobTicket {
        BlobTicket {
            addr: sample_addr(3),
            secret: [9u8; SECRET_LEN],
            sha256: [7u8; HASH_LEN],
            size: 1_234_567,
            lookups: LookupOpts::public_preset(),
            password,
        }
    }

    #[test]
    fn ticket_round_trips() {
        let ticket = sample(false);
        let encoded = ticket.encode();
        assert!(encoded.starts_with(PREFIX));
        let decoded = BlobTicket::decode(&encoded).expect("decode");
        assert_eq!(decoded.addr.id, ticket.addr.id);
        assert_eq!(decoded.secret, [9u8; SECRET_LEN]);
        assert_eq!(decoded.sha256, [7u8; HASH_LEN]);
        assert_eq!(decoded.size, 1_234_567);
        assert_eq!(decoded.lookups, LookupOpts::public_preset());
        assert!(!decoded.password);
    }

    #[test]
    fn password_flag_round_trips() {
        let decoded = BlobTicket::decode(&sample(true).encode()).expect("decode");
        assert!(decoded.password);
    }

    #[test]
    fn decode_tolerates_missing_variation_selector() {
        // A paste may drop the VS-16 that follows the base `🎟` glyph; the
        // VS-stripped form must still decode.
        let encoded = sample(false).encode();
        let bare = encoded.replacen('\u{FE0F}', "", 1);
        assert_ne!(bare, encoded, "encode should emit the VS-16");
        assert!(BlobTicket::decode(&bare).is_ok());
    }

    #[test]
    fn sha256_hex_is_the_lowercase_content_hash() {
        // The receiver lands the fetched bytes under this name, so it must equal
        // the producer's spool filename (lowercase hex of the SHA-256).
        let ticket = sample(false); // sha256 == [7u8; HASH_LEN]
        assert_eq!(ticket.sha256_hex(), "07".repeat(HASH_LEN));
    }

    #[test]
    fn rejects_a_mesh_token() {
        // A mesh id carries the `💬` glyph, not the ticket's `🎟️`, so it fails
        // to decode as a blob ticket on the prefix alone.
        let mesh = crate::protocol::mesh::Mesh::new(
            [1u8; 32],
            crate::protocol::mesh::MeshName::new("t").unwrap(),
            crate::protocol::mesh::MeshConfig::loopback(),
        )
        .to_string();
        assert!(BlobTicket::decode(&mesh).is_err());
    }

    #[test]
    fn rejects_bad_checksum() {
        let mut encoded = sample(false).encode();
        let last = encoded.pop().unwrap();
        encoded.push(if last == '1' { '2' } else { '1' });
        assert!(BlobTicket::decode(&encoded).is_err());
    }
}
