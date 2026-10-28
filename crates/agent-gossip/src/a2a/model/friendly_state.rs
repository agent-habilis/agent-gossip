use serde::{Deserialize, Deserializer, Serializer};

use super::TaskState;

/// # Errors
/// Never — serialization of a small enum cannot fail.
pub fn serialize<S: Serializer>(state: &TaskState, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(state.as_str())
}

/// # Errors
/// An unknown friendly state name.
pub fn deserialize<'de, Input: Deserializer<'de>>(
    deserializer: Input,
) -> Result<TaskState, <Input as Deserializer<'de>>::Error> {
    let raw = String::deserialize(deserializer)?;
    TaskState::from_friendly(&raw)
        .ok_or_else(|| serde::de::Error::custom(format!("unknown task state: {raw}")))
}
