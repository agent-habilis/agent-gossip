//! The blob ticket — a bare Base58Check token carrying everything a consumer
//! needs to fetch one content-addressed blob from its producer: the bearer
//! secret, the content hash + size, the mesh's discovery config, and the
//! producer's blob-endpoint address. Payload layout: `secret(32) ‖ flags(1) ‖
//! sha256(32) ‖ size(8, LE) ‖ lookups ‖ address-json` (lookups is
//! self-delimiting, so the address occupies the remainder). Bit 0 of the flags
//! byte marks a password-protected ticket.
//!
//! Wire: bare Base58Check(`version ‖ kind ‖ payload`) with a `SHA256d`
//! checksum. The `kind` byte marks this as a *blob* ticket so a wrong-kind
//! token (invite / bridge) fails cleanly on decode.

use anyhow::{Context, Result, bail};
use iroh::EndpointAddr;
use sha2::{Digest, Sha256};

use crate::protocol::mesh::LookupOpts;
use crate::protocol::peer_addr::{endpoint_addr_from_json, endpoint_addr_to_json};

use super::{HASH_LEN, SECRET_LEN};

/// Framing version. Bumped only on a breaking framing change; an unknown
/// version is rejected on decode.
const VERSION: u8 = 1;

/// Ticket-kind discriminant, framed after [`VERSION`]. Distinct from invite
/// (`3`) and bridge tickets so a wrong-kind token is rejected on decode.
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
    /// Encode as a bare base58 blob ticket.
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
        base58check_encode(&framed)
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

    /// Decode a bare base58 blob ticket.
    ///
    /// # Errors
    /// Bad Base58/checksum, the wrong ticket kind, a bad version, or a
    /// malformed payload.
    pub fn decode(ticket: &str) -> Result<Self> {
        let framed = base58check_decode(ticket.trim())?;
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
    use super::BlobTicket;
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
        assert!(encoded.is_ascii());
        assert!(!encoded.contains("://"));
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
    fn sha256_hex_is_the_lowercase_content_hash() {
        // The receiver lands the fetched bytes under this name, so it must equal
        // the producer's spool filename (lowercase hex of the SHA-256).
        let ticket = sample(false); // sha256 == [7u8; HASH_LEN]
        assert_eq!(ticket.sha256_hex(), "07".repeat(HASH_LEN));
    }

    #[test]
    fn rejects_a_mesh_token() {
        // A mesh id shares the bare-base58 shape but has a different payload;
        // blob decode must refuse it (wrong kind / structure).
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
