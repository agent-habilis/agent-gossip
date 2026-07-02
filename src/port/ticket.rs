//! The port ticket — a `🐝` token of [`TokenType::Port`] carrying everything a
//! consumer needs to dial the producer: the bearer secret, the producer's
//! target ports, the swarm's discovery config, and the producer's address.
//! Payload layout: `secret(32) ‖ count-byte(1) ‖ ports(2×count, BE) ‖ lookups ‖
//! address-json` (lookups is self-delimiting, so the address occupies the
//! remainder). The count byte's **bit 7** marks a password-protected ticket
//! (there is no separate flags byte in this layout), leaving bits 0-6 for the
//! port count — max 127 ports. A port ticket always exposes at least one
//! port; each is multiplexed over the shared connection.

use anyhow::{Context, Result, bail};
use iroh::EndpointAddr;

use crate::protocol::peer_addr::{endpoint_addr_from_json, endpoint_addr_to_json};
use crate::protocol::swarm::LookupOpts;
use crate::protocol::token::{self, TokenType};

use super::SECRET_LEN;

/// A decoded port ticket.
pub(crate) struct PortTicket {
    pub addr: EndpointAddr,
    pub secret: [u8; SECRET_LEN],
    pub lookups: LookupOpts,
    /// The producer's local target ports. Each is multiplexed over the shared
    /// connection; the consumer displays them and picks which to forward.
    pub target_ports: Vec<u16>,
    /// Password-protected: the consumer must present the Argon2id stretch of
    /// the password (salted by `secret`) in each stream header instead of the
    /// raw secret, so the ticket — and any directory ad carrying it — no
    /// longer redeems alone.
    pub password: bool,
}

/// Bit 7 of the count byte: the password flag. Bits 0-6 carry the count.
const PASSWORD_BIT: u8 = 0b1000_0000;

/// Max ports one ticket can expose (the count byte's low 7 bits).
pub(crate) const MAX_TICKET_PORTS: usize = 127;

impl PortTicket {
    /// Encode as a `🐝` token (`type = port`).
    pub(crate) fn encode(&self) -> String {
        let mut payload = Vec::with_capacity(SECRET_LEN + 1 + 2 * self.target_ports.len() + 64);
        payload.extend_from_slice(&self.secret);
        // `listen` caps the list at MAX_TICKET_PORTS; clamp defensively so the
        // count bits can never disagree with the ports that follow, and bit 7
        // stays free for the password flag.
        let count = u8::try_from(self.target_ports.len().min(MAX_TICKET_PORTS))
            .expect("count clamped to fit 7 bits");
        payload.push(count | if self.password { PASSWORD_BIT } else { 0 });
        for port in self.target_ports.iter().take(usize::from(count)) {
            payload.extend_from_slice(&port.to_be_bytes());
        }
        self.lookups.encode_into(&mut payload);
        let addr_json = serde_json::to_vec(&endpoint_addr_to_json(&self.addr))
            .expect("EndpointAddr JSON always serializes");
        payload.extend_from_slice(&addr_json);
        token::encode(TokenType::Port, &payload)
    }

    /// Decode a `🐝` port ticket.
    ///
    /// # Errors
    /// Not a `🐝` token, the wrong token type, or a malformed payload.
    pub(crate) fn decode(ticket: &str) -> Result<Self> {
        let (kind, payload) = token::decode(ticket.trim())?;
        if kind != TokenType::Port {
            bail!("not a port ticket: wrong token type");
        }
        let secret_slice = payload.get(..SECRET_LEN).context("ticket too short")?;
        let mut secret = [0u8; SECRET_LEN];
        secret.copy_from_slice(secret_slice);
        let count_byte = *payload
            .get(SECRET_LEN)
            .context("ticket missing port count")?;
        let password = count_byte & PASSWORD_BIT != 0;
        let count = usize::from(count_byte & !PASSWORD_BIT);
        let mut pos = SECRET_LEN + 1;
        let mut target_ports = Vec::with_capacity(count);
        for _ in 0..count {
            let port_slice = payload
                .get(pos..pos + 2)
                .context("ticket truncated in port list")?;
            target_ports.push(u16::from_be_bytes([port_slice[0], port_slice[1]]));
            pos += 2;
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
            target_ports,
            password,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{PortTicket, SECRET_LEN};
    use crate::protocol::swarm::LookupOpts;
    use iroh::{EndpointAddr, SecretKey};

    #[test]
    fn ticket_round_trips() {
        let id = SecretKey::from_bytes(&[3u8; 32]).public();
        let addr = EndpointAddr::new(id).with_ip_addr("127.0.0.1:4242".parse().unwrap());
        let ticket = PortTicket {
            addr: addr.clone(),
            secret: [9u8; SECRET_LEN],
            lookups: LookupOpts::public_preset(),
            target_ports: vec![3000],
            password: false,
        };
        let encoded = ticket.encode();
        assert!(encoded.starts_with("🐝"));
        let decoded = PortTicket::decode(&encoded).expect("decode");
        assert_eq!(decoded.addr.id, addr.id);
        assert_eq!(decoded.secret, [9u8; SECRET_LEN]);
        assert_eq!(decoded.lookups, LookupOpts::public_preset());
        assert_eq!(decoded.target_ports, vec![3000]);
        assert!(!decoded.password);
    }

    #[test]
    fn password_flag_round_trips_beside_the_count() {
        let id = SecretKey::from_bytes(&[11u8; 32]).public();
        let addr = EndpointAddr::new(id).with_ip_addr("127.0.0.1:4242".parse().unwrap());
        let ticket = PortTicket {
            addr,
            secret: [9u8; SECRET_LEN],
            lookups: LookupOpts::loopback(),
            target_ports: vec![8080, 5432],
            password: true,
        };
        let decoded = PortTicket::decode(&ticket.encode()).expect("decode");
        assert!(decoded.password);
        assert_eq!(decoded.target_ports, vec![8080, 5432]);
    }

    #[test]
    fn a_max_port_list_still_leaves_the_password_bit_free() {
        let id = SecretKey::from_bytes(&[13u8; 32]).public();
        let addr = EndpointAddr::new(id).with_ip_addr("127.0.0.1:4242".parse().unwrap());
        let ports: Vec<u16> = (1000..1000 + 127).collect();
        let ticket = PortTicket {
            addr,
            secret: [9u8; SECRET_LEN],
            lookups: LookupOpts::loopback(),
            target_ports: ports.clone(),
            password: true,
        };
        let decoded = PortTicket::decode(&ticket.encode()).expect("decode");
        assert!(decoded.password);
        assert_eq!(decoded.target_ports, ports);
    }

    #[test]
    fn target_ports_round_trip() {
        let id = SecretKey::from_bytes(&[7u8; 32]).public();
        let addr = EndpointAddr::new(id).with_ip_addr("127.0.0.1:4242".parse().unwrap());
        let ticket = PortTicket {
            addr,
            secret: [9u8; SECRET_LEN],
            lookups: LookupOpts::loopback(),
            target_ports: vec![8080, 9090, 5432],
            password: false,
        };
        let decoded = PortTicket::decode(&ticket.encode()).expect("decode");
        assert_eq!(decoded.target_ports, vec![8080, 9090, 5432]);
    }

    #[test]
    fn rejects_a_swarm_token() {
        // A `🐝` swarm id is a valid token but the wrong type for a port ticket.
        let swarm = crate::protocol::swarm::Swarm::new(
            [1u8; 32],
            crate::protocol::swarm::SwarmName::new("t").unwrap(),
            crate::protocol::swarm::SwarmConfig::loopback(),
        )
        .to_string();
        assert!(PortTicket::decode(&swarm).is_err());
    }
}
