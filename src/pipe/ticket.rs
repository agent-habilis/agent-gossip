//! The pipe ticket — a `🐝` token of [`TokenType::Pipe`] carrying everything a
//! consumer needs to dial the producer: the bearer secret, a flags byte, the
//! swarm's discovery config, and the producer's address. Payload layout:
//! `secret(32) ‖ flags(1) ‖ lookups ‖ address-json` (lookups is
//! self-delimiting, so the address occupies the remainder). `flags` bit 0 is
//! the live-follow mode, bit 1 is the benchmark protocol (`pipe bench`),
//! bit 2 marks a password-protected ticket (the consumer must present the
//! Argon2id-stretched token, so the ticket alone no longer redeems); the
//! rest are reserved.

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
    /// Live-follow mode: the producer stays up and broadcasts the live source to
    /// every attached consumer at once; a new `pipe connect` joins the fan-out
    /// rather than preempting, and each consumer streams until the source ends.
    pub follow: bool,
    /// A `pipe bench` ticket — the benchmark protocol, not the plain
    /// byte-stream one. Lets `pipe connect` and `pipe bench` each refuse the
    /// other's ticket instead of hanging deep in the wrong protocol.
    pub bench: bool,
    /// Password-protected: the consumer must present the Argon2id stretch of
    /// the password (salted by `secret`) instead of the raw secret, so the
    /// ticket — and any directory ad carrying it — no longer redeems alone.
    pub password: bool,
}

impl PipeTicket {
    /// Encode as a `🐝` token (`type = pipe`).
    pub(crate) fn encode(&self) -> String {
        let mut payload = Vec::with_capacity(SECRET_LEN + 1 + 8 + 64);
        payload.extend_from_slice(&self.secret);
        payload.push(
            u8::from(self.follow) | (u8::from(self.bench) << 1) | (u8::from(self.password) << 2),
        );
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
        let bench = flags & 0b10 != 0;
        let password = flags & 0b100 != 0;
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
            follow,
            bench,
            password,
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
            bench: false,
            password: false,
        };
        let encoded = ticket.encode();
        assert!(encoded.starts_with("🐝"));
        let decoded = PipeTicket::decode(&encoded).expect("decode");
        assert_eq!(decoded.addr.id, addr.id);
        assert_eq!(decoded.secret, [9u8; SECRET_LEN]);
        assert_eq!(decoded.lookups, LookupOpts::public_preset());
        assert!(!decoded.follow);
        assert!(!decoded.bench);
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
            bench: false,
            password: false,
        };
        let decoded = PipeTicket::decode(&ticket.encode()).expect("decode");
        assert!(decoded.follow);
        assert!(!decoded.password);
    }

    #[test]
    fn bench_flag_round_trips_independently_of_follow() {
        let id = SecretKey::from_bytes(&[11u8; 32]).public();
        let addr = EndpointAddr::new(id).with_ip_addr("127.0.0.1:4242".parse().unwrap());
        let ticket = PipeTicket {
            addr,
            secret: [9u8; SECRET_LEN],
            lookups: LookupOpts::loopback(),
            follow: false,
            bench: true,
            password: false,
        };
        let decoded = PipeTicket::decode(&ticket.encode()).expect("decode");
        assert!(decoded.bench);
        assert!(!decoded.follow);
    }

    #[test]
    fn password_flag_round_trips_independently() {
        let id = SecretKey::from_bytes(&[13u8; 32]).public();
        let addr = EndpointAddr::new(id).with_ip_addr("127.0.0.1:4242".parse().unwrap());
        let ticket = PipeTicket {
            addr,
            secret: [9u8; SECRET_LEN],
            lookups: LookupOpts::loopback(),
            follow: false,
            bench: true,
            password: true,
        };
        let decoded = PipeTicket::decode(&ticket.encode()).expect("decode");
        assert!(decoded.password);
        assert!(decoded.bench);
        assert!(!decoded.follow);
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
