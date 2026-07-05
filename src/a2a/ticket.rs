//! The a2a bridge ticket — a `🎟️` token carrying everything a consumer needs
//! to dial the exposer: the bearer secret, the swarm's discovery config, and
//! the exposer's address. Payload layout: `secret(32) ‖ flags(1) ‖ lookups ‖
//! address-json` (lookups is self-delimiting, so the address occupies the
//! remainder). Bit 0 of the flags byte marks a password-protected ticket.
//!
//! Wire: the `🎟️` ticket brand + Base58Check(`version ‖ kind ‖ payload`) with a
//! `SHA256d` checksum; the emoji is the brand, everything after the `://` ASCII
//! Base58. The `🎟️` glyph is distinct from the swarm id's `💬`, so a swarm id
//! fails ticket decode on the prefix alone. The brand *is* shared with the blob
//! ticket, so the `kind` byte marks this as an *a2a bridge* ticket and makes a
//! wrong-kind token (a blob ticket) fail cleanly on decode.

use anyhow::{Context, Result, bail};
use iroh::EndpointAddr;
use sha2::{Digest, Sha256};

use crate::protocol::peer_addr::{endpoint_addr_from_json, endpoint_addr_to_json};
use crate::protocol::swarm::LookupOpts;
use crate::util::consts::{SWARM_URI_SEPARATOR, TICKET_GLYPH};

use super::SECRET_LEN;

/// Branding prefix on every ticket — the `🎟️` ticket glyph; the remainder of the
/// string is ASCII Base58Check. Blob and a2a tickets share this brand and are
/// told apart by [`KIND`]; the swarm id's `💬` is a different glyph entirely.
pub(crate) const PREFIX: &str = TICKET_GLYPH;

/// Framing version. Bumped only on a breaking framing change; an unknown
/// version is rejected on decode.
const VERSION: u8 = 1;

/// Ticket-kind discriminant, framed after [`VERSION`]. Distinct from the blob
/// ticket's kind so a token of the wrong kind is rejected on decode now that
/// both share the `🎟️` brand.
const KIND: u8 = 2;

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
    /// Encode as a `🎟️` a2a bridge token.
    pub(crate) fn encode(&self) -> String {
        let mut payload = Vec::with_capacity(SECRET_LEN + 1 + 64);
        payload.extend_from_slice(&self.secret);
        payload.push(if self.password { PASSWORD_BIT } else { 0 });
        self.lookups.encode_into(&mut payload);
        let addr_json = serde_json::to_vec(&endpoint_addr_to_json(&self.addr))
            .expect("EndpointAddr JSON always serializes");
        payload.extend_from_slice(&addr_json);
        let mut framed = Vec::with_capacity(2 + payload.len());
        framed.push(VERSION);
        framed.push(KIND);
        framed.extend_from_slice(&payload);
        format!("{PREFIX}{SWARM_URI_SEPARATOR}{}", base58check_encode(&framed))
    }

    /// Decode a `🎟️` a2a ticket.
    ///
    /// # Errors
    /// Not a `🎟️` token, the wrong ticket kind, a bad checksum/version, or a
    /// malformed payload.
    pub(crate) fn decode(ticket: &str) -> Result<Self> {
        let body = strip_ticket_prefix(ticket.trim())
            .with_context(|| format!("not an a2a ticket: must start with {PREFIX}"))?;
        let framed = base58check_decode(body)?;
        let version = *framed.first().context("ticket too short")?;
        if version != VERSION {
            bail!("unsupported a2a ticket version: {version}");
        }
        let kind = *framed.get(1).context("ticket too short")?;
        if kind != KIND {
            bail!("not an a2a ticket: wrong ticket kind");
        }
        let payload = &framed[2..];
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

/// Strip the ticket brand and optional `://` off a token, returning the
/// Base58Check body. Accepts the canonical `🎟️://` and, defensively, a paste
/// that dropped the VS-16 (`🎟://`) or the separator — mirroring the swarm id's
/// optional-`://` tolerance. `None` if the token doesn't carry the ticket glyph.
fn strip_ticket_prefix(token: &str) -> Option<&str> {
    let base = TICKET_GLYPH.strip_suffix('\u{FE0F}').unwrap_or(TICKET_GLYPH);
    let rest = token.strip_prefix(base)?;
    let rest = rest.strip_prefix('\u{FE0F}').unwrap_or(rest);
    Some(rest.strip_prefix(SWARM_URI_SEPARATOR).unwrap_or(rest))
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
    fn decode_tolerates_missing_variation_selector() {
        // A paste may drop the VS-16 that follows the base `🎟` glyph; the
        // VS-stripped form must still decode.
        let ticket = A2aTicket {
            addr: sample_addr(7),
            secret: [9u8; SECRET_LEN],
            lookups: LookupOpts::public_preset(),
            password: false,
        };
        let encoded = ticket.encode();
        let bare = encoded.replacen('\u{FE0F}', "", 1);
        assert_ne!(bare, encoded, "encode should emit the VS-16");
        assert!(A2aTicket::decode(&bare).is_ok());
    }

    #[test]
    fn rejects_a_swarm_token() {
        // A swarm id carries the `💬` glyph, not the ticket's `🎟️`, so it fails
        // to decode as an a2a ticket on the prefix alone.
        let swarm = crate::protocol::swarm::Swarm::new(
            [1u8; 32],
            crate::protocol::swarm::SwarmName::new("t").unwrap(),
            crate::protocol::swarm::SwarmConfig::loopback(),
        )
        .to_string();
        assert!(A2aTicket::decode(&swarm).is_err());
    }

    #[test]
    fn rejects_a_cross_kind_ticket() {
        // The blob ticket shares the `🎟️` brand but carries a different kind
        // byte, so it must not decode as an a2a ticket — and vice versa.
        let blob = crate::blob::BlobTicket {
            addr: sample_addr(3),
            secret: [9u8; crate::blob::SECRET_LEN],
            sha256: [7u8; crate::blob::HASH_LEN],
            size: 1_234_567,
            lookups: LookupOpts::public_preset(),
            password: false,
        };
        let a2a = A2aTicket {
            addr: sample_addr(4),
            secret: [9u8; SECRET_LEN],
            lookups: LookupOpts::public_preset(),
            password: false,
        };
        assert!(A2aTicket::decode(&blob.encode()).is_err());
        assert!(crate::blob::BlobTicket::decode(&a2a.encode()).is_err());
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
