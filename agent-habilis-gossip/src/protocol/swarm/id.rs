//! [`SwarmId`] — the validated `💬…` *string* (shallow: prefix +
//! length + Base58 charset). Cheap boundary check at the CLI / IPC
//! edge; full structural decoding lives in [`super::Swarm`].

use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::{PREFIX, SEPARATOR};

const MIN_LEN: usize = 7;
const MAX_LEN: usize = 512;

/// A swarm identifier — the encoded `💬...` Base58Check string.
///
/// Validation is shallow: prefix `💬`, length 7..=512, Base58
/// charset (`[1-9A-HJ-NP-Za-km-z]`). Full structural decoding lives
/// in `Swarm::from_str`; the newtype rejects obvious typos at the
/// CLI / IPC boundary without paying the decode cost on every flow.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SwarmId(String);

/// The pre-rename prefix. An id carrying it was minted by an older,
/// wire-incompatible binary — worth a distinct message so a paster of a
/// stale id isn't left guessing.
const LEGACY_PREFIX: &str = "ahs";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwarmIdError {
    MissingPrefix,
    LegacyPrefix,
    Length(usize),
    Charset(String),
}

impl fmt::Display for SwarmIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SwarmIdError::MissingPrefix => write!(formatter, "swarm id must start with '{PREFIX}'"),
            SwarmIdError::LegacyPrefix => write!(
                formatter,
                "swarm id uses the legacy '{LEGACY_PREFIX}' prefix from an older, incompatible version — ask for a current '{PREFIX}' id"
            ),
            SwarmIdError::Length(len) => {
                write!(
                    formatter,
                    "swarm id must be {MIN_LEN}..={MAX_LEN} chars, got {len}"
                )
            }
            SwarmIdError::Charset(value) => {
                write!(formatter, "swarm id has invalid Base58 char(s): {value:?}")
            }
        }
    }
}

impl std::error::Error for SwarmIdError {}

fn is_base58_char(ch: char) -> bool {
    matches!(ch,
        '1'..='9'
        | 'A'..='H' | 'J'..='N' | 'P'..='Z'
        | 'a'..='k' | 'm'..='z'
    )
}

impl SwarmId {
    /// # Errors
    /// Returns an error if the inputs are invalid or the operation fails.
    pub fn new(value: impl Into<String>) -> Result<Self, SwarmIdError> {
        let value = value.into();
        let Some(rest) = value.strip_prefix(PREFIX) else {
            if value.starts_with(LEGACY_PREFIX) {
                return Err(SwarmIdError::LegacyPrefix);
            }
            return Err(SwarmIdError::MissingPrefix);
        };
        // The `://` is optional on input; normalize both `💬<payload>` and
        // `💬://<payload>` to the canonical form below.
        let payload = rest.strip_prefix(SEPARATOR).unwrap_or(rest);
        // Length is measured on the bare `💬<payload>` form so the bounds
        // don't shift with the optional separator.
        let bare_len = PREFIX.len() + payload.len();
        if !(MIN_LEN..=MAX_LEN).contains(&bare_len) {
            return Err(SwarmIdError::Length(bare_len));
        }
        if !payload.chars().all(is_base58_char) {
            return Err(SwarmIdError::Charset(value));
        }
        Ok(Self(format!("{PREFIX}{SEPARATOR}{payload}")))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SwarmId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for SwarmId {
    type Err = SwarmIdError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::new(text)
    }
}

impl AsRef<str> for SwarmId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for SwarmId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
impl From<&str> for SwarmId {
    fn from(text: &str) -> Self {
        Self::new(text).expect("invalid swarm id in test fixture")
    }
}

#[cfg(test)]
mod swarm_id_tests {
    use super::{SwarmId, SwarmIdError};

    #[test]
    fn new_accepts_well_formed_id() {
        SwarmId::new("💬AbCdEf1234").unwrap();
    }

    #[test]
    fn new_normalizes_to_canonical_uri_form() {
        // Bare and `💬://` inputs collapse to the same canonical string.
        let bare = SwarmId::new("💬AbCdEf1234").unwrap();
        let uri = SwarmId::new("💬://AbCdEf1234").unwrap();
        assert_eq!(bare.as_str(), "💬://AbCdEf1234");
        assert_eq!(bare, uri);
    }

    #[test]
    fn new_rejects_missing_prefix() {
        assert!(matches!(
            SwarmId::new("noprefix12345"),
            Err(SwarmIdError::MissingPrefix)
        ));
    }

    #[test]
    fn new_flags_legacy_ahs_prefix() {
        let err = SwarmId::new("ahsAbCdEf1234").unwrap_err();
        assert_eq!(err, SwarmIdError::LegacyPrefix);
        assert!(err.to_string().contains("legacy"));
    }

    #[test]
    fn new_rejects_too_short() {
        assert!(matches!(SwarmId::new("💬a"), Err(SwarmIdError::Length(_))));
    }

    #[test]
    fn new_rejects_invalid_base58_chars() {
        // `0`, `O`, `I`, `l` are not in the Base58 alphabet.
        assert!(matches!(
            SwarmId::new("💬AbCdEf0xyz"),
            Err(SwarmIdError::Charset(_))
        ));
    }

    #[test]
    fn serde_transparent_round_trip() {
        let id = SwarmId::from("💬AbCdEf1234");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"💬://AbCdEf1234\"");
        let parsed: SwarmId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, id);
    }
}
