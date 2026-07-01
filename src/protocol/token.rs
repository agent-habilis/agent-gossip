//! The branded `🐝` token codec shared by the swarm id ([`super::swarm`]),
//! the pipe ticket ([`crate::pipe`]), and the port ticket ([`crate::port`]).
//! One wire shape for every agent-habilis token, so a `🐝…` string
//! self-describes its kind via a 1-byte type tag and the namespaces never
//! collide.
//!
//! Wire: `🐝` + Base58Check(`version ‖ type ‖ payload`) with a `SHA256d`
//! checksum. The emoji is the brand; everything after it is ASCII Base58
//! (so `util::swarm_prefix` strips the `🐝` to keep file paths ASCII).

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

/// Branding prefix on every token — a single emoji (4 UTF-8 bytes); the
/// remainder of the string is ASCII Base58Check.
pub(crate) const PREFIX: &str = "🐝";

/// Token framing version. Bumped only on a breaking framing change; an
/// unknown version is rejected on decode.
const VERSION: u8 = 1;

/// Which kind of token this is — the byte that lets one `🐝…` namespace
/// carry both swarm ids and pipe tickets without ambiguity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenType {
    Swarm,
    Pipe,
    Port,
    File,
}

impl TokenType {
    fn to_byte(self) -> u8 {
        match self {
            TokenType::Swarm => 1,
            TokenType::Pipe => 2,
            TokenType::Port => 3,
            TokenType::File => 4,
        }
    }

    fn from_byte(byte: u8) -> Result<Self> {
        match byte {
            1 => Ok(TokenType::Swarm),
            2 => Ok(TokenType::Pipe),
            3 => Ok(TokenType::Port),
            4 => Ok(TokenType::File),
            other => bail!("unknown token type: {other}"),
        }
    }
}

/// Encode `payload` as a `🐝` token of the given `kind`.
pub(crate) fn encode(kind: TokenType, payload: &[u8]) -> String {
    let mut framed = Vec::with_capacity(2 + payload.len());
    framed.push(VERSION);
    framed.push(kind.to_byte());
    framed.extend_from_slice(payload);
    format!("{PREFIX}{}", base58check_encode(&framed))
}

/// Decode a `🐝` token into its kind and raw payload, validating the
/// prefix, the Base58Check checksum, and the version byte.
///
/// # Errors
/// A missing/wrong `🐝` prefix, invalid Base58, a bad checksum, an unknown
/// version, or an unknown type byte.
pub(crate) fn decode(token: &str) -> Result<(TokenType, Vec<u8>)> {
    let body = token
        .strip_prefix(PREFIX)
        .context("token must start with 🐝")?;
    let framed = base58check_decode(body)?;
    let version = *framed.first().context("token too short")?;
    if version != VERSION {
        bail!("unsupported token version: {version}");
    }
    let kind = TokenType::from_byte(*framed.get(1).context("token too short")?)?;
    Ok((kind, framed[2..].to_vec()))
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
        .context("invalid Base58 token encoding")?;
    if decoded.len() < 4 {
        bail!("token too short");
    }
    let (payload, received) = decoded.split_at(decoded.len() - 4);
    if received != checksum(payload) {
        bail!("invalid token checksum");
    }
    Ok(payload.to_vec())
}

#[cfg(test)]
mod tests {
    use super::{TokenType, VERSION, base58check_encode, decode, encode};

    #[test]
    fn round_trips_each_kind() {
        for kind in [
            TokenType::Swarm,
            TokenType::Pipe,
            TokenType::Port,
            TokenType::File,
        ] {
            let token = encode(kind, b"payload-bytes");
            assert!(token.starts_with("🐝"));
            let (decoded_kind, payload) = decode(&token).expect("decode");
            assert_eq!(decoded_kind, kind);
            assert_eq!(payload, b"payload-bytes");
        }
    }

    #[test]
    fn rejects_missing_prefix() {
        let token = encode(TokenType::Swarm, b"x");
        let body = token.strip_prefix("🐝").unwrap();
        assert!(decode(body).is_err(), "a bare body has no 🐝 prefix");
    }

    #[test]
    fn rejects_bad_checksum() {
        let mut token = encode(TokenType::Swarm, b"payload");
        let last = token.pop().unwrap();
        token.push(if last == '1' { '2' } else { '1' });
        assert!(decode(&token).is_err());
    }

    #[test]
    fn rejects_unknown_version() {
        let token = format!("🐝{}", base58check_encode(&[9u8, 1u8, 0u8]));
        assert!(decode(&token).is_err());
    }

    #[test]
    fn rejects_unknown_type() {
        let token = format!("🐝{}", base58check_encode(&[VERSION, 9u8, 0u8]));
        assert!(decode(&token).is_err());
    }
}
