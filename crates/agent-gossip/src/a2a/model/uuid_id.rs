use std::fmt;
use std::marker::PhantomData;

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

/// The kind of a UUID-string id: names the error text a crafted id gets.
pub trait IdKind {
    const ERROR: &'static str;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageIdKind {}

impl IdKind for MessageIdKind {
    const ERROR: &'static str = "invalid a2a message id";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskIdKind {}

impl IdKind for TaskIdKind {
    const ERROR: &'static str = "invalid a2a task id";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum A2aRpcIdKind {}

impl IdKind for A2aRpcIdKind {
    const ERROR: &'static str = "invalid a2a rpc id";
}

/// A UUID-string id newtype with validating deserialize, so a crafted
/// non-UUID id is rejected at parse. One generic, three ids: the sender-minted
/// message id, the server-minted task id, and the gossip RPC correlation id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct UuidId<Kind: IdKind>(String, #[serde(skip)] PhantomData<Kind>);

pub type MessageId = UuidId<MessageIdKind>;
pub type TaskId = UuidId<TaskIdKind>;
pub type A2aRpcId = UuidId<A2aRpcIdKind>;

impl<'de, Kind: IdKind> Deserialize<'de> for UuidId<Kind> {
    fn deserialize<Input>(deserializer: Input) -> Result<Self, Input::Error>
    where
        Input: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Uuid::parse_str(&raw).map_err(|_| serde::de::Error::custom(Kind::ERROR))?;
        Ok(Self(raw, PhantomData))
    }
}

impl<Kind: IdKind> UuidId<Kind> {
    #[must_use]
    pub fn random() -> Self {
        Self(Uuid::new_v4().to_string(), PhantomData)
    }

    /// Adopt an existing UUID string (e.g. a frame id, which shares
    /// the UUID form). `None` for a non-UUID.
    #[must_use]
    pub fn from_uuid_str(raw: &str) -> Option<Self> {
        Uuid::parse_str(raw)
            .ok()
            .map(|_| Self(raw.to_string(), PhantomData))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<Kind: IdKind> fmt::Display for UuidId<Kind> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<Kind: IdKind> std::str::FromStr for UuidId<Kind> {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::from_uuid_str(raw).ok_or_else(|| Kind::ERROR.to_string())
    }
}

#[cfg(test)]
impl<Kind: IdKind> From<&str> for UuidId<Kind> {
    fn from(raw: &str) -> Self {
        Uuid::parse_str(raw).expect(Kind::ERROR);
        Self(raw.to_string(), PhantomData)
    }
}
