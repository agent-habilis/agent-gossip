//! The a2a bridge ticket — a `📡` token carrying everything a consumer needs
//! to dial the exposer: the bearer secret, the swarm's discovery config, and
//! the exposer's address. Payload layout: `secret(32) ‖ flags(1) ‖ lookups ‖
//! address-json` (lookups is self-delimiting, so the address occupies the
//! remainder). Bit 0 of the flags byte marks a password-protected ticket.
//!
//! The token is its own namespace (`📡`, distinct from the swarm id's `🐝`), so
//! there is no type byte — the whole payload is one shape. Wire: `📡` +
//! Base58Check(`version ‖ payload`) with a `SHA256d` checksum; the emoji is the
//! brand, everything after it ASCII Base58.

use anyhow::{Context, Result, bail};
use iroh::EndpointAddr;
use sha2::{Digest, Sha256};

use crate::protocol::peer_addr::{endpoint_addr_from_json, endpoint_addr_to_json};
use crate::protocol::swarm::LookupOpts;

use super::SECRET_LEN;

/// Branding prefix on every a2a ticket — a single emoji (4 UTF-8 bytes); the
/// remainder of the string is ASCII Base58Check.
pub(crate) const PREFIX: &str = "📡";

/// Framing version. Bumped only on a breaking framing change; an unknown
/// version is rejected on decode.
const VERSION: u8 = 1;

/// Bit 0 of the flags byte: the password flag.
const PASSWORD_BIT: u8 = 0b0000_0001;

/// A decoded a2a bridge ticket.
pub(crate) struct A2aTicket {
    pub addr: EndpointAddr,
    pub secret: [u8; SECRET_LEN],
    pub lookups: LookupOpts,
    /// Password-protected: the consumer must present the Argon2id stretch of
    /// the password (salted by `secret`) in the stream header instead of the
    /// raw secret, so the ticket — and any directory ad carrying it — no longer
    /// redeems alone.
    pub password: bool,
}

impl A2aTicket {
    /// Encode as a `📡` token.
    pub(crate) fn encode(&self) -> String {
        let mut payload = Vec::with_capacity(SECRET_LEN + 1 + 64);
        payload.extend_from_slice(&self.secret);
        payload.push(if self.password { PASSWORD_BIT } else { 0 });
        self.lookups.encode_into(&mut payload);
        let addr_json = serde_json::to_vec(&endpoint_addr_to_json(&self.addr))
            .expect("EndpointAddr JSON always serializes");
        payload.extend_from_slice(&addr_json);
        let mut framed = Vec::with_capacity(1 + payload.len());
        framed.push(VERSION);
        framed.extend_from_slice(&payload);
        format!("{PREFIX}{}", base58check_encode(&framed))
    }

    /// Decode a `📡` a2a ticket.
    ///
    /// # Errors
    /// Not a `📡` token, a bad checksum/version, or a malformed payload.
    pub(crate) fn decode(ticket: &str) -> Result<Self> {
        let body = ticket
            .trim()
            .strip_prefix(PREFIX)
            .context("not an a2a ticket: must start with 📡")?;
        let framed = base58check_decode(body)?;
        let version = *framed.first().context("ticket too short")?;
        if version != VERSION {
            bail!("unsupported a2a ticket version: {version}");
        }
        let payload = &framed[1..];
        let secret_slice = payload.get(..SECRET_LEN).context("ticket too short")?;
        let mut secret = [0u8; SECRET_LEN];
        secret.copy_from_slice(secret_slice);
        let flags = *payload.get(SECRET_LEN).context("ticket missing flags")?;
        let password = flags & PASSWORD_BIT != 0;
        let mut pos = SECRET_LEN + 1;
        let lookups = LookupOpts::decode_from(payload, &mut pos)?;
        let addr_json = payload.get(pos..).context("ticket missing address")?;
        let value: serde_json::Value =
            serde_json::from_slice(addr_json).context("invalid ticket address json")?;
        let (_id, addr) = endpoint_addr_from_json(&value)?;
        Ok(Self {
            addr,
            secret,
            lookups,
            password,
        })
    }
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
        .context("invalid Base58 in a2a ticket")?;
    if decoded.len() < 4 {
        bail!("a2a ticket too short");
    }
    let (payload, received) = decoded.split_at(decoded.len() - 4);
    if received != checksum(payload) {
        bail!("invalid a2a ticket checksum");
    }
    Ok(payload.to_vec())
}

#[cfg(test)]
mod tests {
    use super::{A2aTicket, PREFIX, SECRET_LEN};
    use crate::protocol::swarm::LookupOpts;
    use iroh::{EndpointAddr, SecretKey};

    fn sample_addr(byte: u8) -> EndpointAddr {
        let id = SecretKey::from_bytes(&[byte; 32]).public();
        EndpointAddr::new(id).with_ip_addr("127.0.0.1:4242".parse().unwrap())
    }

    #[test]
    fn ticket_round_trips() {
        let addr = sample_addr(3);
        let ticket = A2aTicket {
            addr: addr.clone(),
            secret: [9u8; SECRET_LEN],
            lookups: LookupOpts::public_preset(),
            password: false,
        };
        let encoded = ticket.encode();
        assert!(encoded.starts_with(PREFIX));
        let decoded = A2aTicket::decode(&encoded).expect("decode");
        assert_eq!(decoded.addr.id, addr.id);
        assert_eq!(decoded.secret, [9u8; SECRET_LEN]);
        assert_eq!(decoded.lookups, LookupOpts::public_preset());
        assert!(!decoded.password);
    }

    #[test]
    fn password_flag_round_trips() {
        let ticket = A2aTicket {
            addr: sample_addr(11),
            secret: [9u8; SECRET_LEN],
            lookups: LookupOpts::loopback(),
            password: true,
        };
        let decoded = A2aTicket::decode(&ticket.encode()).expect("decode");
        assert!(decoded.password);
    }

    #[test]
    fn rejects_a_swarm_token() {
        // A `🐝` swarm id is a valid token but the wrong brand for an a2a ticket.
        let swarm = crate::protocol::swarm::Swarm::new(
            [1u8; 32],
            crate::protocol::swarm::SwarmName::new("t").unwrap(),
            crate::protocol::swarm::SwarmConfig::loopback(),
        )
        .to_string();
        assert!(A2aTicket::decode(&swarm).is_err());
    }

    #[test]
    fn rejects_bad_checksum() {
        let ticket = A2aTicket {
            addr: sample_addr(5),
            secret: [1u8; SECRET_LEN],
            lookups: LookupOpts::loopback(),
            password: false,
        };
        let mut encoded = ticket.encode();
        let last = encoded.pop().unwrap();
        encoded.push(if last == '1' { '2' } else { '1' });
        assert!(A2aTicket::decode(&encoded).is_err());
    }
}
