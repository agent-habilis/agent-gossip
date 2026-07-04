//! Multipart bodies: a logical message whose body exceeds the single-message
//! wire cap ([`MAX_MESSAGE_SIZE`](crate::util::consts::MAX_MESSAGE_SIZE)) is
//! split by the sender into several ordinary signed messages, each carrying a
//! [`Shard`] header. The receiver reassembles them; the split is invisible to
//! agents on both ends. Each shard is a real message (own id/seq/signature) and
//! lives in the message log like any other, so anti-entropy heals a missing
//! shard exactly as it heals a missing message.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use crate::util::consts::MAX_MESSAGE_SHARDS;

/// The correlation id shared by every shard of one logical body — a UUID v4
/// string form, minted once by the sender when it splits a body. Like
/// [`TaskId`](crate::a2a::TaskId), deserialization is **validating**, so a
/// non-UUID group is rejected at `Message::parse`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ShardGroup(String);

impl<'de> Deserialize<'de> for ShardGroup {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Uuid::parse_str(&raw).map_err(|_| serde::de::Error::custom("invalid shard group"))?;
        Ok(Self(raw))
    }
}

impl ShardGroup {
    /// Adopt an existing UUID string as the group id — the chat path names a
    /// split body's group by its payload's A2A `messageId`, so the reassembled
    /// logical id *is* the A2A id. `None` for a non-UUID.
    #[must_use]
    pub(crate) fn from_uuid_str(raw: &str) -> Option<Self> {
        Uuid::parse_str(raw).ok().map(|_| Self(raw.to_string()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ShardGroup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The per-message header on one shard of a split body: its correlation
/// [`group`](Shard::group), 0-based index, and the total shard count. Present
/// only on the messages of a multipart body; ordinary messages carry no
/// `shard`. Deserialization is **validating** — `total` must be `2..=`
/// [`MAX_MESSAGE_SHARDS`] and `idx < total` — so a crafted shard that would over-
/// allocate or never complete is rejected at `Message::parse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Shard {
    pub group: ShardGroup,
    pub idx: u32,
    pub total: u32,
}

impl<'de> Deserialize<'de> for Shard {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            group: ShardGroup,
            idx: u32,
            total: u32,
        }
        let raw = Raw::deserialize(deserializer)?;
        // A single message never carries `shard` (the sender only splits when a
        // body won't fit), so the smallest real `total` is 2; the upper bound
        // caps reassembly buffering at a crafted peer's whim.
        let max = u32::try_from(MAX_MESSAGE_SHARDS).unwrap_or(u32::MAX);
        if raw.total < 2 || raw.total > max {
            return Err(serde::de::Error::custom("shard total out of range"));
        }
        if raw.idx >= raw.total {
            return Err(serde::de::Error::custom("shard idx out of range"));
        }
        Ok(Shard {
            group: raw.group,
            idx: raw.idx,
            total: raw.total,
        })
    }
}

#[cfg(test)]
mod shard_tests {
    use super::{Shard, ShardGroup};

    const GROUP: &str = "550e8400-e29b-41d4-a716-446655440000";

    #[test]
    fn group_deserialize_rejects_non_uuid() {
        assert!(serde_json::from_str::<ShardGroup>("\"not-a-uuid\"").is_err());
        assert!(ShardGroup::from_uuid_str("not-a-uuid").is_none());
    }

    #[test]
    fn shard_rejects_out_of_range_total_and_idx() {
        let group = ShardGroup::from_uuid_str(GROUP).expect("valid group");
        let group = group.as_str();
        // total < 2 (a single shard is never multipart).
        assert!(
            serde_json::from_str::<Shard>(&format!(r#"{{"group":"{group}","idx":0,"total":1}}"#))
                .is_err()
        );
        // total over the cap would over-allocate the reassembly window.
        assert!(
            serde_json::from_str::<Shard>(&format!(
                r#"{{"group":"{group}","idx":0,"total":9999}}"#
            ))
            .is_err()
        );
        // idx must be inside total.
        assert!(
            serde_json::from_str::<Shard>(&format!(r#"{{"group":"{group}","idx":3,"total":3}}"#))
                .is_err()
        );
    }

    #[test]
    fn shard_accepts_valid_header() {
        let group = ShardGroup::from_uuid_str(GROUP).expect("valid group");
        let group = group.as_str();
        let shard: Shard =
            serde_json::from_str(&format!(r#"{{"group":"{group}","idx":1,"total":3}}"#)).unwrap();
        assert_eq!(shard.idx, 1);
        assert_eq!(shard.total, 3);
    }
}
