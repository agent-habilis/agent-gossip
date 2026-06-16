//! [`TaskId`] — a task-exchange correlation identifier (UUID v4 string
//! form). A task is a directed, multi-leg exchange between two
//! participants; every leg carries the same `TaskId` so both sides (and
//! the daemon's per-task state machine + debounce timer) can group the
//! legs into one conversation. Distinct newtype from [`MessageId`](super::MessageId)
//! so the two UUIDs — one per *message*, one per *task* — never get
//! swapped (the same discipline that keeps `id` and `after` apart).

use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

/// A task correlation id — UUID v4 string form. Minted once by the
/// initiator (`random`) and echoed on every leg of that task. Like
/// [`MessageId`](super::MessageId), deserialization is **validating**, so a
/// non-UUID `task_id` is rejected at `Message::parse` rather than flowing
/// into the state machine.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct TaskId(String);

impl<'de> Deserialize<'de> for TaskId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        TaskId::new(raw).map_err(serde::de::Error::custom)
    }
}

/// Error parsing a [`TaskId`] from a non-UUID string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskIdError(String);

impl fmt::Display for TaskIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid task id: {:?}", self.0)
    }
}

impl std::error::Error for TaskIdError {}

impl TaskId {
    pub(crate) fn new(value: impl Into<String>) -> Result<Self, TaskIdError> {
        let value = value.into();
        Uuid::parse_str(&value).map_err(|_| TaskIdError(value.clone()))?;
        Ok(Self(value))
    }

    /// Mint a fresh v4 id. In production the *initiator* (the skill/agent)
    /// mints the `task_id` and passes it in via `--task-id`; the daemon only
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

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for TaskId {
    type Err = TaskIdError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::new(text)
    }
}

impl AsRef<str> for TaskId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for TaskId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
impl From<&str> for TaskId {
    fn from(text: &str) -> Self {
        Self::new(text).expect("invalid task id in test fixture")
    }
}

#[cfg(test)]
mod task_id_tests {
    use super::TaskId;

    #[test]
    fn new_accepts_uuid_v4() {
        let id = TaskId::new("550e8400-e29b-41d4-a716-446655440000").unwrap();
        assert_eq!(id.as_str(), "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn new_rejects_garbage() {
        assert!(TaskId::new("not-a-uuid").is_err());
        assert!(TaskId::new("").is_err());
    }

    #[test]
    fn random_round_trips_through_new() {
        let id = TaskId::random();
        TaskId::new(id.as_str()).expect("random must round-trip through new");
    }

    #[test]
    fn deserialize_rejects_non_uuid() {
        assert!(serde_json::from_str::<TaskId>("\"not-a-uuid\"").is_err());
        assert!(serde_json::from_str::<TaskId>("\"\"").is_err());
    }
}
