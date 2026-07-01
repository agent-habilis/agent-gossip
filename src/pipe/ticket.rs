//! The pipe ticket — a `🐝` token of [`TokenType::Pipe`] carrying everything a
//! consumer needs to dial the producer: the bearer secret, a flags byte, the
//! target port, the swarm's discovery config, and the producer's address.
//! Payload layout: `secret(32) ‖ flags(1) ‖ target_port(2) ‖ lookups ‖
//! address-json` (lookups is self-delimiting, so the address occupies the
//! remainder). `flags` bit 0 is the live-follow mode; the rest are reserved.
//! `target_port` is a big-endian `u16`, `0` meaning "none" (the single-shot
//! stdio pipe has no target port; only `listen-tcp` sets it).

use anyhow::{Context, Result, bail};
use iroh::EndpointAddr;

use crate::protocol::peer_addr::{endpoint_addr_from_json, endpoint_addr_to_json};
use crate::protocol::swarm::LookupOpts;
use crate::protocol::token::{self, TokenType};

use super::SECRET_LEN;

/// A decoded pipe ticket.
pub(crate) struct PipeTicket {
    pub addr: EndpointAddr,
    pub secret: [u8; SECRET_LEN],
    pub lookups: LookupOpts,
    /// Live-follow mode: the producer stays up serving one consumer at a time,
    /// and the consumer streams-and-dies (a reconnect re-runs `pipe connect`).
    pub follow: bool,
    /// The producer's local target port (`listen-tcp` only — `None` for the
    /// single-shot stdio pipe), so the consumer can display it.
    pub target_port: Option<u16>,
}

impl PipeTicket {
    /// Encode as a `🐝` token (`type = pipe`).
    pub(crate) fn encode(&self) -> String {
        let mut payload = Vec::with_capacity(SECRET_LEN + 1 + 2 + 8 + 64);
        payload.extend_from_slice(&self.secret);
        payload.push(u8::from(self.follow));
        payload.extend_from_slice(&self.target_port.unwrap_or(0).to_be_bytes());
        self.lookups.encode_into(&mut payload);
        let addr_json = serde_json::to_vec(&endpoint_addr_to_json(&self.addr))
            .expect("EndpointAddr JSON always serializes");
        payload.extend_from_slice(&addr_json);
        token::encode(TokenType::Pipe, &payload)
    }

    /// Decode a `🐝` pipe ticket.
    ///
    /// # Errors
    /// Not a `🐝` token, the wrong token type, or a malformed payload.
    pub(crate) fn decode(ticket: &str) -> Result<Self> {
        let (kind, payload) = token::decode(ticket.trim())?;
        if kind != TokenType::Pipe {
            bail!("not a pipe ticket: wrong token type");
        }
        let secret_slice = payload.get(..SECRET_LEN).context("ticket too short")?;
        let mut secret = [0u8; SECRET_LEN];
        secret.copy_from_slice(secret_slice);
        let flags = *payload.get(SECRET_LEN).context("ticket missing flags")?;
        let follow = flags & 1 != 0;
        let port_slice = payload
            .get(SECRET_LEN + 1..SECRET_LEN + 3)
            .context("ticket missing target port")?;
        let target_port = match u16::from_be_bytes([port_slice[0], port_slice[1]]) {
            0 => None,
            port => Some(port),
        };
        let mut pos = SECRET_LEN + 3;
        let lookups = LookupOpts::decode_from(&payload, &mut pos)?;
        let addr_json = payload.get(pos..).context("ticket missing address")?;
        let value: serde_json::Value =
            serde_json::from_slice(addr_json).context("invalid ticket address json")?;
        let (_id, addr) = endpoint_addr_from_json(&value)?;
        Ok(Self {
            addr,
            secret,
            lookups,
            follow,
            target_port,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{PipeTicket, SECRET_LEN};
    use crate::protocol::swarm::LookupOpts;
    use iroh::{EndpointAddr, SecretKey};

    #[test]
    fn ticket_round_trips() {
        let id = SecretKey::from_bytes(&[3u8; 32]).public();
        let addr = EndpointAddr::new(id).with_ip_addr("127.0.0.1:4242".parse().unwrap());
        let ticket = PipeTicket {
            addr: addr.clone(),
            secret: [9u8; SECRET_LEN],
            lookups: LookupOpts::public_preset(),
            follow: false,
            target_port: None,
        };
        let encoded = ticket.encode();
        assert!(encoded.starts_with("🐝"));
        let decoded = PipeTicket::decode(&encoded).expect("decode");
        assert_eq!(decoded.addr.id, addr.id);
        assert_eq!(decoded.secret, [9u8; SECRET_LEN]);
        assert_eq!(decoded.lookups, LookupOpts::public_preset());
        assert!(!decoded.follow);
        assert_eq!(decoded.target_port, None);
    }

    #[test]
    fn follow_flag_round_trips() {
        let id = SecretKey::from_bytes(&[5u8; 32]).public();
        let addr = EndpointAddr::new(id).with_ip_addr("127.0.0.1:4242".parse().unwrap());
        let ticket = PipeTicket {
            addr,
            secret: [9u8; SECRET_LEN],
            lookups: LookupOpts::loopback(),
            follow: true,
            target_port: None,
        };
        let decoded = PipeTicket::decode(&ticket.encode()).expect("decode");
        assert!(decoded.follow);
    }

    #[test]
    fn target_port_round_trips() {
        let id = SecretKey::from_bytes(&[7u8; 32]).public();
        let addr = EndpointAddr::new(id).with_ip_addr("127.0.0.1:4242".parse().unwrap());
        let ticket = PipeTicket {
            addr,
            secret: [9u8; SECRET_LEN],
            lookups: LookupOpts::loopback(),
            follow: false,
            target_port: Some(8080),
        };
        let decoded = PipeTicket::decode(&ticket.encode()).expect("decode");
        assert_eq!(decoded.target_port, Some(8080));
    }

    #[test]
    fn rejects_a_swarm_token() {
        // A `🐝` swarm id is a valid token but the wrong type for a pipe.
        let swarm = crate::protocol::swarm::Swarm::new(
            [1u8; 32],
            crate::protocol::swarm::SwarmName::new("t").unwrap(),
            crate::protocol::swarm::SwarmConfig::loopback(),
        )
        .to_string();
        assert!(PipeTicket::decode(&swarm).is_err());
    }
}
