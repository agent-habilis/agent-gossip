//! [`MeshId`] — the validated `💬…` *string* (shallow: prefix +
//! length + Base58 charset). Cheap boundary check at the CLI / IPC
//! edge; full structural decoding lives in [`super::Mesh`].

use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::{PREFIX, SEPARATOR};

const MIN_LEN: usize = 7;
const MAX_LEN: usize = 512;

/// A mesh identifier — the encoded `💬...` Base58Check string.
///
/// Validation is shallow: prefix `💬`, length 7..=512, Base58
/// charset (`[1-9A-HJ-NP-Za-km-z]`). Full structural decoding lives
/// in `Mesh::from_str`; the newtype rejects obvious typos at the
/// CLI / IPC boundary without paying the decode cost on every flow.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MeshId(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeshIdError {
    MissingPrefix,
    Length(usize),
    Charset(String),
}

impl fmt::Display for MeshIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MeshIdError::MissingPrefix => write!(formatter, "mesh id must start with '{PREFIX}'"),
            MeshIdError::Length(len) => {
                write!(
                    formatter,
                    "mesh id must be {MIN_LEN}..={MAX_LEN} chars, got {len}"
                )
            }
            MeshIdError::Charset(value) => {
                write!(formatter, "mesh id has invalid Base58 char(s): {value:?}")
            }
        }
    }
}

impl std::error::Error for MeshIdError {}

fn is_base58_char(ch: char) -> bool {
    matches!(ch,
        '1'..='9'
        | 'A'..='H' | 'J'..='N' | 'P'..='Z'
        | 'a'..='k' | 'm'..='z'
    )
}

impl MeshId {
    /// # Errors
    /// Returns an error if the inputs are invalid or the operation fails.
    pub fn new(value: impl Into<String>) -> Result<Self, MeshIdError> {
        let value = value.into();
        let Some(rest) = value.strip_prefix(PREFIX) else {
            return Err(MeshIdError::MissingPrefix);
        };
        // The `://` is optional on input; normalize both `💬<payload>` and
        // `💬://<payload>` to the canonical form below.
        let payload = rest.strip_prefix(SEPARATOR).unwrap_or(rest);
        // Length is measured on the bare `💬<payload>` form so the bounds
        // don't shift with the optional separator.
        let bare_len = PREFIX.len() + payload.len();
        if !(MIN_LEN..=MAX_LEN).contains(&bare_len) {
            return Err(MeshIdError::Length(bare_len));
        }
        if !payload.chars().all(is_base58_char) {
            return Err(MeshIdError::Charset(value));
        }
        Ok(Self(format!("{PREFIX}{SEPARATOR}{payload}")))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MeshId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for MeshId {
    type Err = MeshIdError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::new(text)
    }
}

impl AsRef<str> for MeshId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for MeshId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
impl From<&str> for MeshId {
    fn from(text: &str) -> Self {
        Self::new(text).expect("invalid mesh id in test fixture")
    }
}

#[cfg(test)]
mod mesh_id_tests {
    use super::{MeshId, MeshIdError};

    #[test]
    fn new_accepts_well_formed_id() {
        MeshId::new("💬AbCdEf1234").unwrap();
    }

    #[test]
    fn new_normalizes_to_canonical_uri_form() {
        // Bare and `💬://` inputs collapse to the same canonical string.
        let bare = MeshId::new("💬AbCdEf1234").unwrap();
        let uri = MeshId::new("💬://AbCdEf1234").unwrap();
        assert_eq!(bare.as_str(), "💬://AbCdEf1234");
        assert_eq!(bare, uri);
    }

    #[test]
    fn new_rejects_missing_prefix() {
        assert!(matches!(
            MeshId::new("noprefix12345"),
            Err(MeshIdError::MissingPrefix)
        ));
    }

    #[test]
    fn new_rejects_too_short() {
        assert!(matches!(MeshId::new("💬a"), Err(MeshIdError::Length(_))));
    }

    #[test]
    fn new_rejects_invalid_base58_chars() {
        // `0`, `O`, `I`, `l` are not in the Base58 alphabet.
        assert!(matches!(
            MeshId::new("💬AbCdEf0xyz"),
            Err(MeshIdError::Charset(_))
        ));
    }

    #[test]
    fn serde_transparent_round_trip() {
        let id = MeshId::from("💬AbCdEf1234");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"💬://AbCdEf1234\"");
        let parsed: MeshId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, id);
    }
}
