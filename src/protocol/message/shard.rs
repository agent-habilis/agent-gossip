//! Multipart bodies: a logical message whose body exceeds the single-message
//! wire cap ([`MAX_MESSAGE_SIZE`](crate::util::consts::MAX_MESSAGE_SIZE)) is
//! split by the sender into several ordinary signed messages, each carrying a
//! [`Shard`] header. The receiver reassembles them; the split is invisible to
//! agents on both ends. Each shard is a real message (own id/seq/signature);
//! shards of a small group (`total <=`
//! [`LOGGED_SHARD_GROUP_MAX_TOTAL`](crate::util::consts::LOGGED_SHARD_GROUP_MAX_TOTAL))
//! also live in the message log, so anti-entropy heals them exactly as it
//! heals a missing message; a bigger group skips the log on both ends (one
//! huge body must not evict the swarm's anti-entropy history) and heals via
//! **shard repair** — the sender caches its outbound frames
//! (`daemon::reassembly::ShardCache`) and a stalled receiver asks for the
//! missing indexes over the `shard/repair` gossip RPC.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use crate::util::consts::MAX_SHARD_TOTAL;

/// Whether a frame belongs in the message log as far as sharding is
/// concerned: unsharded frames and shards of a small group do; a big group's
/// shards stay out on **both** ends. The gate must be two-sided — a sender
/// that logs what receivers refuse to retain makes every anti-entropy digest
/// advertise those shards as missing, and the sender re-floods them every
/// round while the resend budget starves real gaps.
pub(crate) fn shard_fits_log(msg: &super::Message) -> bool {
    msg.shard
        .as_ref()
        .is_none_or(|shard| shard.total <= crate::util::consts::LOGGED_SHARD_GROUP_MAX_TOTAL)
}

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
/// [`MAX_SHARD_TOTAL`] and `idx < total` — an absurd-header tripwire, not a
/// memory guard: the reassembly store is byte-budgeted and never allocates by
/// `total`.
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
        // rejects a header no budget-respecting sender can produce.
        if raw.total < 2 || raw.total > MAX_SHARD_TOTAL {
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
        // A large-but-sane total is fine (the store is byte-budgeted)...
        assert!(
            serde_json::from_str::<Shard>(&format!(
                r#"{{"group":"{group}","idx":0,"total":9999}}"#
            ))
            .is_ok()
        );
        // ...but an absurd header past the tripwire is rejected.
        assert!(
            serde_json::from_str::<Shard>(&format!(
                r#"{{"group":"{group}","idx":0,"total":{}}}"#,
                u64::from(super::MAX_SHARD_TOTAL) + 1
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
    fn shard_total_tripwire_clears_the_reassembly_budget() {
        // The tripwire must never reject a body the byte budgets allow: even
        // at a pessimistic ~2 KiB of body per shard, a group at the byte
        // ceiling needs fewer shards than the header bound admits.
        let worst_case_shards = crate::util::consts::REASSEMBLY_GROUP_MAX_BYTES.div_ceil(2048);
        assert!(
            worst_case_shards <= super::MAX_SHARD_TOTAL as usize,
            "MAX_SHARD_TOTAL ({}) must clear REASSEMBLY_GROUP_MAX_BYTES / 2KiB ({worst_case_shards})",
            super::MAX_SHARD_TOTAL
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
