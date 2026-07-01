use anyhow::{Context, Result, bail};
use iroh::EndpointAddr;

use crate::protocol::peer_addr::{endpoint_addr_from_json, endpoint_addr_to_json};
use crate::protocol::swarm::LookupOpts;
use crate::protocol::token::{self, TokenType};

use super::SECRET_LEN;

/// A decoded mount ticket — the bearer secret, the swarm's discovery config,
/// and the producer's address. Payload layout mirrors the file ticket:
/// `secret(32) ‖ flags(1) ‖ lookups ‖ address-json` (lookups is
/// self-delimiting, so the address occupies the remainder). `flags` is
/// reserved for forward-compat and always 0 today.
pub(crate) struct MountTicket {
    pub addr: EndpointAddr,
    pub secret: [u8; SECRET_LEN],
    pub lookups: LookupOpts,
}

impl MountTicket {
    /// Encode as a `🐝` token (`type = mount`).
    pub(crate) fn encode(&self) -> String {
        let mut payload = Vec::with_capacity(SECRET_LEN + 1 + 64);
        payload.extend_from_slice(&self.secret);
        payload.push(0); // reserved flags byte
        self.lookups.encode_into(&mut payload);
        let addr_json = serde_json::to_vec(&endpoint_addr_to_json(&self.addr))
            .expect("EndpointAddr JSON always serializes");
        payload.extend_from_slice(&addr_json);
        token::encode(TokenType::Mount, &payload)
    }

    /// Decode a `🐝` mount ticket.
    ///
    /// # Errors
    /// Not a `🐝` token, the wrong token type, or a malformed payload.
    pub(crate) fn decode(ticket: &str) -> Result<Self> {
        let (kind, payload) = token::decode(ticket.trim())?;
        if kind != TokenType::Mount {
            bail!("not a mount ticket: wrong token type");
        }
        let secret_slice = payload.get(..SECRET_LEN).context("ticket too short")?;
        let mut secret = [0u8; SECRET_LEN];
        secret.copy_from_slice(secret_slice);
        // Skip the reserved flags byte.
        let mut pos = SECRET_LEN + 1;
        if payload.len() < pos {
            bail!("ticket missing flags");
        }
        let lookups = LookupOpts::decode_from(&payload, &mut pos)?;
        let addr_json = payload.get(pos..).context("ticket missing address")?;
        let value: serde_json::Value =
            serde_json::from_slice(addr_json).context("invalid ticket address json")?;
        let (_id, addr) = endpoint_addr_from_json(&value)?;
        Ok(Self {
            addr,
            secret,
            lookups,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{MountTicket, SECRET_LEN};
    use crate::protocol::swarm::LookupOpts;
    use iroh::{EndpointAddr, SecretKey};

    #[test]
    fn ticket_round_trips() {
        let id = SecretKey::from_bytes(&[7u8; 32]).public();
        let addr = EndpointAddr::new(id).with_ip_addr("127.0.0.1:4242".parse().unwrap());
        let ticket = MountTicket {
            addr: addr.clone(),
            secret: [5u8; SECRET_LEN],
            lookups: LookupOpts::public_preset(),
        };
        let encoded = ticket.encode();
        assert!(encoded.starts_with("🐝"));
        let decoded = MountTicket::decode(&encoded).expect("decode");
        assert_eq!(decoded.addr.id, addr.id);
        assert_eq!(decoded.secret, [5u8; SECRET_LEN]);
        assert_eq!(decoded.lookups, LookupOpts::public_preset());
    }

    #[test]
    fn rejects_a_swarm_token() {
        // A `🐝` swarm id is a valid token but the wrong type for a mount ticket.
        let swarm = crate::protocol::swarm::Swarm::new(
            [1u8; 32],
            crate::protocol::swarm::SwarmName::new("t").unwrap(),
            crate::protocol::swarm::SwarmConfig::loopback(),
        )
        .to_string();
        assert!(MountTicket::decode(&swarm).is_err());
    }
}
