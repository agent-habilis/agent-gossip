//! [`MessageId`] — a protocol message identifier (UUID v4 string form).
//! The newtype prevents argument-order confusion between `id` and
//! `after`-cursor parameters that carry the same kind of value.

use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

/// A protocol message identifier — UUID v4 string form.
///
/// Construction goes through `new` (validates UUID format) or `random`
/// (mints a fresh v4). The newtype prevents argument-order confusion
/// between `id` and `after`-cursor parameters that carry the same kind
/// of value. Deserialization is **validating** (not the derived
/// transparent one): an inbound id off the wire is run through `new`, so a
/// non-UUID `id` is rejected at `Message::parse` rather than panicking later
/// in `as_uuid_bytes` (the anti-entropy digest path).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct MessageId(String);

impl<'de> Deserialize<'de> for MessageId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        MessageId::new(raw).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdError(String);

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid message id: {:?}", self.0)
    }
}

impl std::error::Error for IdError {}

impl MessageId {
    pub(crate) fn new(value: impl Into<String>) -> Result<Self, IdError> {
        let value = value.into();
        Uuid::parse_str(&value).map_err(|_| IdError(value.clone()))?;
        Ok(Self(value))
    }

    pub(crate) fn random() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The id's 16 raw UUID bytes — the compact form packed into an
    /// anti-entropy digest (vs the 36-char string), so far more ids fit
    /// one gossip message. Infallible: a `MessageId` is always a valid
    /// UUID (enforced by `new`).
    pub(crate) fn as_uuid_bytes(&self) -> [u8; 16] {
        Uuid::parse_str(&self.0)
            .expect("MessageId always holds a valid UUID")
            .into_bytes()
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for MessageId {
    type Err = IdError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::new(text)
    }
}

impl AsRef<str> for MessageId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for MessageId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
impl From<&str> for MessageId {
    fn from(text: &str) -> Self {
        Self::new(text).expect("invalid message id in test fixture")
    }
}

#[cfg(test)]
mod id_tests {
    use super::MessageId;

    #[test]
    fn new_accepts_uuid_v4() {
        let id = MessageId::new("550e8400-e29b-41d4-a716-446655440000").unwrap();
        assert_eq!(id.as_str(), "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn new_rejects_garbage() {
        assert!(MessageId::new("not-a-uuid").is_err());
        assert!(MessageId::new("").is_err());
        assert!(MessageId::new("550e8400").is_err());
    }

    #[test]
    fn random_produces_valid_id() {
        let first = MessageId::random();
        let second = MessageId::random();
        assert_ne!(first, second);
        MessageId::new(first.as_str()).expect("random must round-trip through new");
    }

    #[test]
    fn from_str_works_for_clap() {
        let id: MessageId = "550e8400-e29b-41d4-a716-446655440000".parse().unwrap();
        assert_eq!(id.as_str(), "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn serde_transparent_round_trip() {
        let id = MessageId::random();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{}\"", id.as_str()));
        let parsed: MessageId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn deserialize_rejects_non_uuid() {
        // The derived transparent Deserialize would accept any string and let
        // `as_uuid_bytes` panic later; the validating impl rejects it here.
        assert!(serde_json::from_str::<MessageId>("\"not-a-uuid\"").is_err());
        assert!(serde_json::from_str::<MessageId>("\"550e8400\"").is_err());
        assert!(serde_json::from_str::<MessageId>("\"\"").is_err());
    }
}
