use anyhow::{Context, Result, bail};
use iroh::EndpointAddr;

use crate::protocol::peer_addr::{endpoint_addr_from_json, endpoint_addr_to_json};
use crate::protocol::swarm::LookupOpts;
use crate::protocol::token::{self, TokenType};

use super::SECRET_LEN;

/// A decoded shell ticket — a `🐝` token of [`TokenType::Sh`] carrying
/// everything a viewer needs to dial the producer. Payload layout:
/// `secret(32) ‖ flags(1) ‖ lookups ‖ address-json` (lookups is
/// self-delimiting, so the address occupies the remainder). `flags` bit 0
/// marks a write-capable ticket — UX only, so the viewer knows to forward its
/// keyboard; the producer grants write by which secret was presented, never by
/// this flag. Bit 1 marks a password-protected ticket, so the viewer knows to
/// present the password. The other bits stay reserved for forward-compat.
pub(crate) struct ShTicket {
    pub addr: EndpointAddr,
    pub secret: [u8; SECRET_LEN],
    pub lookups: LookupOpts,
    pub write: bool,
    pub password: bool,
}

impl ShTicket {
    /// Encode as a `🐝` token (`type = sh`).
    pub(crate) fn encode(&self) -> String {
        let mut payload = Vec::with_capacity(SECRET_LEN + 1 + 64);
        payload.extend_from_slice(&self.secret);
        payload.push(u8::from(self.write) | (u8::from(self.password) << 1));
        self.lookups.encode_into(&mut payload);
        let addr_json = serde_json::to_vec(&endpoint_addr_to_json(&self.addr))
            .expect("EndpointAddr JSON always serializes");
        payload.extend_from_slice(&addr_json);
        token::encode(TokenType::Sh, &payload)
    }

    /// Decode a `🐝` shell ticket.
    ///
    /// # Errors
    /// Not a `🐝` token, the wrong token type, or a malformed payload.
    pub(crate) fn decode(ticket: &str) -> Result<Self> {
        let (kind, payload) = token::decode(ticket.trim())?;
        if kind != TokenType::Sh {
            bail!("not a shell ticket: wrong token type");
        }
        let secret_slice = payload.get(..SECRET_LEN).context("ticket too short")?;
        let mut secret = [0u8; SECRET_LEN];
        secret.copy_from_slice(secret_slice);
        // Bit 0 marks a write ticket, bit 1 a password-protected one; the rest
        // stay reserved.
        let flags = *payload.get(SECRET_LEN).context("ticket missing flags")?;
        let write = flags & 0x01 != 0;
        let password = flags & 0x02 != 0;
        let mut pos = SECRET_LEN + 1;
        let lookups = LookupOpts::decode_from(&payload, &mut pos)?;
        let addr_json = payload.get(pos..).context("ticket missing address")?;
        let value: serde_json::Value =
            serde_json::from_slice(addr_json).context("invalid ticket address json")?;
        let (_id, addr) = endpoint_addr_from_json(&value)?;
        Ok(Self {
            addr,
            secret,
            lookups,
            write,
            password,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{SECRET_LEN, ShTicket};
    use crate::protocol::swarm::LookupOpts;
    use iroh::{EndpointAddr, SecretKey};

    #[test]
    fn ticket_round_trips() {
        let id = SecretKey::from_bytes(&[3u8; 32]).public();
        let addr = EndpointAddr::new(id).with_ip_addr("127.0.0.1:4242".parse().unwrap());
        let ticket = ShTicket {
            addr: addr.clone(),
            secret: [9u8; SECRET_LEN],
            lookups: LookupOpts::public_preset(),
            write: false,
            password: false,
        };
        let encoded = ticket.encode();
        assert!(encoded.starts_with("🐝"));
        let decoded = ShTicket::decode(&encoded).expect("decode");
        assert_eq!(decoded.addr.id, addr.id);
        assert_eq!(decoded.secret, [9u8; SECRET_LEN]);
        assert_eq!(decoded.lookups, LookupOpts::public_preset());
        assert!(!decoded.write);
        assert!(!decoded.password);
    }

    #[test]
    fn write_flag_round_trips() {
        let id = SecretKey::from_bytes(&[4u8; 32]).public();
        let addr = EndpointAddr::new(id).with_ip_addr("127.0.0.1:4243".parse().unwrap());
        let ticket = ShTicket {
            addr,
            secret: [7u8; SECRET_LEN],
            lookups: LookupOpts::public_preset(),
            write: true,
            password: false,
        };
        let decoded = ShTicket::decode(&ticket.encode()).expect("decode");
        assert!(decoded.write);
        assert!(!decoded.password);
    }

    #[test]
    fn password_flag_round_trips() {
        let id = SecretKey::from_bytes(&[5u8; 32]).public();
        let addr = EndpointAddr::new(id).with_ip_addr("127.0.0.1:4244".parse().unwrap());
        // A write + passworded ticket: both flag bits ride the same byte
        // independently.
        let ticket = ShTicket {
            addr,
            secret: [6u8; SECRET_LEN],
            lookups: LookupOpts::public_preset(),
            write: true,
            password: true,
        };
        let decoded = ShTicket::decode(&ticket.encode()).expect("decode");
        assert!(decoded.write);
        assert!(decoded.password);
    }

    #[test]
    fn rejects_a_swarm_token() {
        // A `🐝` swarm id is a valid token but the wrong type for a shell ticket.
        let swarm = crate::protocol::swarm::Swarm::new(
            [1u8; 32],
            crate::protocol::swarm::SwarmName::new("t").unwrap(),
            crate::protocol::swarm::SwarmConfig::loopback(),
        )
        .to_string();
        assert!(ShTicket::decode(&swarm).is_err());
    }
}
