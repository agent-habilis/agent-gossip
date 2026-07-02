//! The file ticket — a `🐝` token of [`TokenType::File`] carrying everything a
//! consumer needs to dial the producer: the bearer secret, a reserved flags
//! byte, the swarm's discovery config, and the producer's address. Payload
//! layout: `secret(32) ‖ flags(1) ‖ lookups ‖ address-json` (lookups is
//! self-delimiting, so the address occupies the remainder). `flags` bit 0
//! marks a password-protected ticket (the consumer must present the Argon2id
//! stretch of the password instead of the raw secret); the rest are reserved.

use anyhow::{Context, Result, bail};
use iroh::EndpointAddr;

use crate::protocol::peer_addr::{endpoint_addr_from_json, endpoint_addr_to_json};
use crate::protocol::swarm::LookupOpts;
use crate::protocol::token::{self, TokenType};

use super::SECRET_LEN;

/// A decoded file ticket.
pub(crate) struct FileTicket {
    pub addr: EndpointAddr,
    pub secret: [u8; SECRET_LEN],
    pub lookups: LookupOpts,
    /// Password-protected: the consumer must present the Argon2id stretch of
    /// the password (salted by `secret`) instead of the raw secret, so the
    /// ticket — and any directory ad carrying it — no longer redeems alone.
    pub password: bool,
}

impl FileTicket {
    /// Encode as a `🐝` token (`type = file`).
    pub(crate) fn encode(&self) -> String {
        let mut payload = Vec::with_capacity(SECRET_LEN + 1 + 64);
        payload.extend_from_slice(&self.secret);
        payload.push(u8::from(self.password));
        self.lookups.encode_into(&mut payload);
        let addr_json = serde_json::to_vec(&endpoint_addr_to_json(&self.addr))
            .expect("EndpointAddr JSON always serializes");
        payload.extend_from_slice(&addr_json);
        token::encode(TokenType::File, &payload)
    }

    /// Decode a `🐝` file ticket.
    ///
    /// # Errors
    /// Not a `🐝` token, the wrong token type, or a malformed payload.
    pub(crate) fn decode(ticket: &str) -> Result<Self> {
        let (kind, payload) = token::decode(ticket.trim())?;
        if kind != TokenType::File {
            bail!("not a file ticket: wrong token type");
        }
        let secret_slice = payload.get(..SECRET_LEN).context("ticket too short")?;
        let mut secret = [0u8; SECRET_LEN];
        secret.copy_from_slice(secret_slice);
        let flags = *payload.get(SECRET_LEN).context("ticket missing flags")?;
        let password = flags & 1 != 0;
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
            password,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{FileTicket, SECRET_LEN};
    use crate::protocol::swarm::LookupOpts;
    use iroh::{EndpointAddr, SecretKey};

    #[test]
    fn ticket_round_trips() {
        let id = SecretKey::from_bytes(&[3u8; 32]).public();
        let addr = EndpointAddr::new(id).with_ip_addr("127.0.0.1:4242".parse().unwrap());
        let ticket = FileTicket {
            addr: addr.clone(),
            secret: [9u8; SECRET_LEN],
            lookups: LookupOpts::public_preset(),
            password: false,
        };
        let encoded = ticket.encode();
        assert!(encoded.starts_with("🐝"));
        let decoded = FileTicket::decode(&encoded).expect("decode");
        assert_eq!(decoded.addr.id, addr.id);
        assert_eq!(decoded.secret, [9u8; SECRET_LEN]);
        assert_eq!(decoded.lookups, LookupOpts::public_preset());
        assert!(!decoded.password);
    }

    #[test]
    fn password_flag_round_trips() {
        let id = SecretKey::from_bytes(&[7u8; 32]).public();
        let addr = EndpointAddr::new(id).with_ip_addr("127.0.0.1:4242".parse().unwrap());
        let ticket = FileTicket {
            addr,
            secret: [9u8; SECRET_LEN],
            lookups: LookupOpts::loopback(),
            password: true,
        };
        let decoded = FileTicket::decode(&ticket.encode()).expect("decode");
        assert!(decoded.password);
    }

    #[test]
    fn rejects_a_swarm_token() {
        // A `🐝` swarm id is a valid token but the wrong type for a file ticket.
        let swarm = crate::protocol::swarm::Swarm::new(
            [1u8; 32],
            crate::protocol::swarm::SwarmName::new("t").unwrap(),
            crate::protocol::swarm::SwarmConfig::loopback(),
        )
        .to_string();
        assert!(FileTicket::decode(&swarm).is_err());
    }
}
