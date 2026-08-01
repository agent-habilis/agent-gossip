//! [`MeshId`] — the validated bare Base58Check mesh string. Raw strings stay
//! outside the type until their checksum and complete payload have been
//! validated.

use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

use super::Mesh;

const MIN_LEN: usize = 7;
const MAX_LEN: usize = 512;

/// A mesh identifier — the encoded bare Base58Check string.
///
/// Construction validates the length, Base58 charset, checksum, version, and
/// complete payload. Consequently every `MeshId` can be decoded as a [`Mesh`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct MeshId(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeshIdError {
    Length(usize),
    Charset(String),
    InvalidHash,
}

impl fmt::Display for MeshIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MeshIdError::Length(len) => {
                write!(
                    formatter,
                    "mesh id must be {MIN_LEN}..={MAX_LEN} chars, got {len}"
                )
            }
            MeshIdError::Charset(value) => {
                write!(formatter, "mesh id has invalid Base58 char(s): {value:?}")
            }
            MeshIdError::InvalidHash => formatter.write_str("invalid gossip hash"),
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
        let len = value.len();
        if !(MIN_LEN..=MAX_LEN).contains(&len) {
            return Err(MeshIdError::Length(len));
        }
        if !value.chars().all(is_base58_char) {
            return Err(MeshIdError::Charset(value));
        }
        value
            .parse::<Mesh>()
            .map_err(|_| MeshIdError::InvalidHash)?;
        Ok(Self(value))
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

impl TryFrom<String> for MeshId {
    type Error = MeshIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for MeshId {
    fn deserialize<DeserializerT>(deserializer: DeserializerT) -> Result<Self, DeserializerT::Error>
    where
        DeserializerT: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
impl From<&str> for MeshId {
    fn from(label: &str) -> Self {
        if let Ok(id) = Self::new(label) {
            return id;
        }
        let mesh = Mesh::from_topic(label, super::MeshConfig::loopback());
        Self::new(mesh.to_string()).expect("generated test mesh id must be valid")
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

#[cfg(test)]
mod mesh_id_tests {
    use super::{Mesh, MeshId, MeshIdError};

    fn valid_id() -> String {
        use super::super::{MeshConfig, MeshName};
        Mesh::new(
            [1u8; 32],
            MeshName::new("test").unwrap(),
            MeshConfig::loopback(),
        )
        .to_string()
    }

    #[test]
    fn new_accepts_well_formed_id() {
        MeshId::new(valid_id()).unwrap();
    }

    #[test]
    fn new_rejects_too_short() {
        assert!(matches!(MeshId::new("a"), Err(MeshIdError::Length(_))));
    }

    #[test]
    fn new_rejects_invalid_base58_chars() {
        // `0`, `O`, `I`, `l` are not in the Base58 alphabet.
        assert!(matches!(
            MeshId::new("AbCdEf0xyzZZ"),
            Err(MeshIdError::Charset(_))
        ));
    }

    #[test]
    fn serde_transparent_round_trip() {
        let id_str = valid_id();
        let id = MeshId::new(id_str.clone()).unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id_str}\""));
        let parsed: MeshId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn new_rejects_a_mistyped_hash() {
        let mut mistyped = valid_id();
        let replacement = if mistyped.ends_with('1') { "2" } else { "1" };
        mistyped.replace_range(mistyped.len() - 1.., replacement);
        assert_eq!(MeshId::new(mistyped), Err(MeshIdError::InvalidHash));
    }

    #[test]
    fn serde_rejects_an_invalid_hash() {
        let invalid = serde_json::to_string("AbCdEf1234567").unwrap();
        let error = serde_json::from_str::<MeshId>(&invalid).unwrap_err();
        assert!(error.to_string().contains("invalid gossip hash"));
    }

    #[test]
    fn every_mesh_id_decodes_as_a_mesh() {
        let id = MeshId::new(valid_id()).unwrap();
        id.as_str().parse::<Mesh>().unwrap();
    }

    #[test]
    fn canonical_form_has_no_uri_wrapping() {
        let id = valid_id();
        assert!(!id.contains("://"), "got {id}");
        assert!(!id.contains('💬'), "got {id}");
    }
}
