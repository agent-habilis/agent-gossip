//! [`ExchangeId`] — an exchange correlation identifier (UUID v4 string
//! form). An exchange is a directed, multi-leg conversation between two
//! participants; every leg carries the same `ExchangeId` so both sides (and
//! the daemon's per-exchange state machine + debounce timer) can group the
//! legs into one conversation. Distinct newtype from [`MessageId`](super::MessageId)
//! so the two UUIDs — one per *message*, one per *exchange* — never get
//! swapped (the same discipline that keeps `id` and `after` apart).

use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

/// An exchange correlation id — UUID v4 string form. Minted once by the
/// initiator (`random`) and echoed on every leg of that exchange. Like
/// [`MessageId`](super::MessageId), deserialization is **validating**, so a
/// non-UUID `exchange_id` is rejected at `Message::parse` rather than flowing
/// into the state machine.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ExchangeId(String);

impl<'de> Deserialize<'de> for ExchangeId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        ExchangeId::new(raw).map_err(serde::de::Error::custom)
    }
}

/// Error parsing a [`ExchangeId`] from a non-UUID string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExchangeIdError(String);

impl fmt::Display for ExchangeIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid exchange id: {:?}", self.0)
    }
}

impl std::error::Error for ExchangeIdError {}

impl ExchangeId {
    pub(crate) fn new(value: impl Into<String>) -> Result<Self, ExchangeIdError> {
        let value = value.into();
        Uuid::parse_str(&value).map_err(|_| ExchangeIdError(value.clone()))?;
        Ok(Self(value))
    }

    /// Mint a fresh v4 id. In production the *initiator* (the skill/agent)
    /// mints the `exchange_id` and passes it in via `--exchange-id`; the daemon only
    /// ever echoes a received id, so this is used by tests only.
    #[cfg(test)]
    pub(crate) fn random() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ExchangeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ExchangeId {
    type Err = ExchangeIdError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::new(text)
    }
}

impl AsRef<str> for ExchangeId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for ExchangeId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
impl From<&str> for ExchangeId {
    fn from(text: &str) -> Self {
        Self::new(text).expect("invalid exchange id in test fixture")
    }
}

#[cfg(test)]
mod exchange_id_tests {
    use super::ExchangeId;

    #[test]
    fn new_accepts_uuid_v4() {
        let id = ExchangeId::new("550e8400-e29b-41d4-a716-446655440000").unwrap();
        assert_eq!(id.as_str(), "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn new_rejects_garbage() {
        assert!(ExchangeId::new("not-a-uuid").is_err());
        assert!(ExchangeId::new("").is_err());
    }

    #[test]
    fn random_round_trips_through_new() {
        let id = ExchangeId::random();
        ExchangeId::new(id.as_str()).expect("random must round-trip through new");
    }

    #[test]
    fn deserialize_rejects_non_uuid() {
        assert!(serde_json::from_str::<ExchangeId>("\"not-a-uuid\"").is_err());
        assert!(serde_json::from_str::<ExchangeId>("\"\"").is_err());
    }
}
