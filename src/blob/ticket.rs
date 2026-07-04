//! The blob ticket — a `📦` token carrying everything a consumer needs to fetch
//! one content-addressed blob from its producer: the bearer secret, the content
//! hash + size, the swarm's discovery config, and the producer's blob-endpoint
//! address. Payload layout: `secret(32) ‖ flags(1) ‖ sha256(32) ‖ size(8, LE) ‖
//! lookups ‖ address-json` (lookups is self-delimiting, so the address occupies
//! the remainder). Bit 0 of the flags byte marks a password-protected ticket.
//!
//! Its own namespace (`📦`, distinct from the swarm id's `🐝` and the a2a
//! bridge's `📡`), so there is no type byte. Wire: `📦` + Base58Check(`version ‖
//! payload`) with a `SHA256d` checksum — the emoji is the brand, the remainder
//! ASCII Base58. Mirrors [`crate::a2a::ticket`].

use anyhow::{Context, Result, bail};
use iroh::EndpointAddr;
use sha2::{Digest, Sha256};

use crate::protocol::peer_addr::{endpoint_addr_from_json, endpoint_addr_to_json};
use crate::protocol::swarm::LookupOpts;

use super::{HASH_LEN, SECRET_LEN};

/// Branding prefix on every blob ticket — a single emoji (4 UTF-8 bytes); the
/// remainder of the string is ASCII Base58Check.
pub(crate) const PREFIX: &str = "📦";

/// Framing version. Bumped only on a breaking framing change; an unknown
/// version is rejected on decode.
const VERSION: u8 = 1;

/// Bit 0 of the flags byte: the password flag.
const PASSWORD_BIT: u8 = 0b0000_0001;

/// A decoded blob ticket.
pub(crate) struct BlobTicket {
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
    /// Encode as a `📦` token.
    pub(crate) fn encode(&self) -> String {
        let mut payload = Vec::with_capacity(SECRET_LEN + 1 + HASH_LEN + 8 + 64);
        payload.extend_from_slice(&self.secret);
        payload.push(if self.password { PASSWORD_BIT } else { 0 });
        payload.extend_from_slice(&self.sha256);
        payload.extend_from_slice(&self.size.to_le_bytes());
        self.lookups.encode_into(&mut payload);
        let addr_json = serde_json::to_vec(&endpoint_addr_to_json(&self.addr))
            .expect("EndpointAddr JSON always serializes");
        payload.extend_from_slice(&addr_json);
        let mut framed = Vec::with_capacity(1 + payload.len());
        framed.push(VERSION);
        framed.extend_from_slice(&payload);
        format!("{PREFIX}{}", base58check_encode(&framed))
    }

    /// Decode a `📦` blob ticket.
    ///
    /// # Errors
    /// Not a `📦` token, a bad checksum/version, or a malformed payload.
    pub(crate) fn decode(ticket: &str) -> Result<Self> {
        let body = ticket
            .trim()
            .strip_prefix(PREFIX)
            .context("not a blob ticket: must start with 📦")?;
        let framed = base58check_decode(body)?;
        let version = *framed.first().context("ticket too short")?;
        if version != VERSION {
            bail!("unsupported blob ticket version: {version}");
        }
        let payload = &framed[1..];
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
    use super::{BlobTicket, PREFIX};
    use crate::blob::{HASH_LEN, SECRET_LEN};
    use crate::protocol::swarm::LookupOpts;
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
    fn rejects_a_swarm_token() {
        // A `🐝` swarm id is a valid token but the wrong brand for a blob ticket.
        let swarm = crate::protocol::swarm::Swarm::new(
            [1u8; 32],
            crate::protocol::swarm::SwarmName::new("t").unwrap(),
            crate::protocol::swarm::SwarmConfig::loopback(),
        )
        .to_string();
        assert!(BlobTicket::decode(&swarm).is_err());
    }

    #[test]
    fn rejects_bad_checksum() {
        let mut encoded = sample(false).encode();
        let last = encoded.pop().unwrap();
        encoded.push(if last == '1' { '2' } else { '1' });
        assert!(BlobTicket::decode(&encoded).is_err());
    }
}
