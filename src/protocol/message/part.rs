//! Multipart bodies: a logical message whose body exceeds the single-message
//! wire cap ([`MAX_MESSAGE_SIZE`](crate::util::consts::MAX_MESSAGE_SIZE)) is
//! split by the sender into several ordinary signed messages, each carrying a
//! [`Part`] header. The receiver reassembles them; the split is invisible to
//! agents on both ends. Each part is a real message (own id/seq/signature) and
//! lives in the message log like any other, so anti-entropy heals a missing
//! part exactly as it heals a missing message.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use crate::util::consts::MAX_MESSAGE_PARTS;

/// The correlation id shared by every part of one logical body — a UUID v4
/// string form, minted once by the sender when it splits a body. Like
/// [`ExchangeId`](super::ExchangeId), deserialization is **validating**, so a
/// non-UUID group is rejected at `Message::parse`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct PartGroup(String);

impl<'de> Deserialize<'de> for PartGroup {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Uuid::parse_str(&raw).map_err(|_| serde::de::Error::custom("invalid part group"))?;
        Ok(Self(raw))
    }
}

impl PartGroup {
    /// Mint a fresh v4 group id for a new split.
    #[must_use]
    pub(crate) fn random() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PartGroup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The per-message header on one part of a split body: its correlation
/// [`group`](Part::group), 0-based index, and the total part count. Present
/// only on the messages of a multipart body; ordinary messages carry no
/// `part`. Deserialization is **validating** — `total` must be `2..=`
/// [`MAX_MESSAGE_PARTS`] and `idx < total` — so a crafted part that would over-
/// allocate or never complete is rejected at `Message::parse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Part {
    pub group: PartGroup,
    pub idx: u32,
    pub total: u32,
}

impl<'de> Deserialize<'de> for Part {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            group: PartGroup,
            idx: u32,
            total: u32,
        }
        let raw = Raw::deserialize(deserializer)?;
        // A single message never carries `part` (the sender only splits when a
        // body won't fit), so the smallest real `total` is 2; the upper bound
        // caps reassembly buffering at a crafted peer's whim.
        let max = u32::try_from(MAX_MESSAGE_PARTS).unwrap_or(u32::MAX);
        if raw.total < 2 || raw.total > max {
            return Err(serde::de::Error::custom("part total out of range"));
        }
        if raw.idx >= raw.total {
            return Err(serde::de::Error::custom("part idx out of range"));
        }
        Ok(Part {
            group: raw.group,
            idx: raw.idx,
            total: raw.total,
        })
    }
}

#[cfg(test)]
mod part_tests {
    use super::{Part, PartGroup};

    #[test]
    fn group_deserialize_rejects_non_uuid() {
        assert!(serde_json::from_str::<PartGroup>("\"not-a-uuid\"").is_err());
    }

    #[test]
    fn part_rejects_out_of_range_total_and_idx() {
        let group = PartGroup::random();
        let group = group.as_str();
        // total < 2 (a single part is never multipart).
        assert!(
            serde_json::from_str::<Part>(&format!(r#"{{"group":"{group}","idx":0,"total":1}}"#))
                .is_err()
        );
        // total over the cap would over-allocate the reassembly window.
        assert!(
            serde_json::from_str::<Part>(&format!(r#"{{"group":"{group}","idx":0,"total":9999}}"#))
                .is_err()
        );
        // idx must be inside total.
        assert!(
            serde_json::from_str::<Part>(&format!(r#"{{"group":"{group}","idx":3,"total":3}}"#))
                .is_err()
        );
    }

    #[test]
    fn part_accepts_valid_header() {
        let group = PartGroup::random();
        let group = group.as_str();
        let part: Part =
            serde_json::from_str(&format!(r#"{{"group":"{group}","idx":1,"total":3}}"#)).unwrap();
        assert_eq!(part.idx, 1);
        assert_eq!(part.total, 3);
    }
}
