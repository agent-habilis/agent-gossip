use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

const MESSAGE_KIND: u8 = 0;
const TASK_KIND: u8 = 1;
const RPC_KIND: u8 = 2;

/// The text of the error that a non-UUID value of this kind gets at parse.
const fn error_text(kind: u8) -> &'static str {
    match kind {
        MESSAGE_KIND => "invalid a2a message id",
        TASK_KIND => "invalid a2a task id",
        RPC_KIND => "invalid a2a rpc id",
        _ => "invalid a2a id",
    }
}

/// A UUID-string id newtype with validating deserialize, so a crafted
/// non-UUID id is rejected at parse. One const-generic type, three ids: the sender-minted
/// message id, the server-minted task id, and the gossip RPC correlation id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct UuidId<const KIND: u8>(String);

/// The id the sender mints for one message.
pub type MessageId = UuidId<MESSAGE_KIND>;
/// The id the server mints for one task.
pub type TaskId = UuidId<TASK_KIND>;
/// The correlation id of one gossip RPC call.
pub type A2aRpcId = UuidId<RPC_KIND>;

impl<'de, const KIND: u8> Deserialize<'de> for UuidId<KIND> {
    fn deserialize<Input>(deserializer: Input) -> Result<Self, Input::Error>
    where
        Input: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Uuid::parse_str(&raw).map_err(|_| serde::de::Error::custom(error_text(KIND)))?;
        Ok(Self(raw))
    }
}

impl<const KIND: u8> UuidId<KIND> {
    #[must_use]
    pub fn random() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// Adopt an existing UUID string (e.g. a frame id, which shares
    /// the UUID form). `None` for a non-UUID.
    #[must_use]
    pub fn from_uuid_str(raw: &str) -> Option<Self> {
        Uuid::parse_str(raw).ok().map(|_| Self(raw.to_string()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<const KIND: u8> fmt::Display for UuidId<KIND> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<const KIND: u8> std::str::FromStr for UuidId<KIND> {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::from_uuid_str(raw).ok_or_else(|| error_text(KIND).to_string())
    }
}

#[cfg(test)]
impl<const KIND: u8> From<&str> for UuidId<KIND> {
    fn from(raw: &str) -> Self {
        Uuid::parse_str(raw).unwrap_or_else(|_| panic!("{}", error_text(KIND)));
        Self(raw.to_string())
    }
}
