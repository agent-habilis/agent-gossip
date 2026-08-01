use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::Instant;

use crate::protocol::{Message, MessageBody, MessageId, MessageKind, Nickname, ShardGroup};
use crate::util::consts::{
    LOGGED_SHARD_GROUP_MAX_TOTAL, REASSEMBLY_AUTHOR_BUDGET_BYTES, REASSEMBLY_GROUP_MAX_BYTES,
    REASSEMBLY_MAX_GROUP_LIFETIME_SECS, REASSEMBLY_REPAIR_IDLE_SECS, REASSEMBLY_REPAIR_MAX_IDXS,
    REASSEMBLY_SLOT_MIN_CHARGE_BYTES, REASSEMBLY_SLOT_OVERHEAD_BYTES, REASSEMBLY_STALE_SECS,
    REASSEMBLY_TOTAL_BUDGET_BYTES, SHARD_CACHE_BUDGET_BYTES,
};

/// What one buffered shard costs against the byte budgets: its body plus the
/// per-slot overhead (map nodes, keys, the shard-0 envelope clone), floored so
/// a 1-byte crafted shard still pays for the ~KB of state it occupies.
pub(crate) fn slot_charge(body_len: usize) -> usize {
    (body_len + REASSEMBLY_SLOT_OVERHEAD_BYTES).max(REASSEMBLY_SLOT_MIN_CHARGE_BYTES)
}

/// `tracing` target for reassembly (matches the owning gossip plane).
const LOG_TARGET: &str = "agent_habilis_mesh::gossip";

/// What became of one ingested shard.
pub(crate) enum ShardIngest {
    /// Buffered; its group is still incomplete.
    Buffered,
    /// The shard completed its group: the synthesized logical message, with
    /// the group's budget already freed. Boxed — the completed arm is rare
    /// and the enum otherwise carries nothing (`large_enum_variant`).
    Complete(Box<Message>),
    /// Dropped — a duplicate slot, a header mismatch, or a budget breach
    /// (which drops the whole group, fail closed).
    Dropped,
}

/// One in-flight multipart body: the buffered shard bodies slotted by `idx`.
///
/// `slots` is a `BTreeMap`, never a `total`-sized allocation — a crafted
/// `total` must cost the sender shards, not us memory. `header` is the idx-0
/// shard, the envelope template `synthesize_logical` builds the view from.
#[derive(Debug)]
struct PartialGroup {
    total: u32,
    /// Kind pinned at the first shard; a later shard of a different kind is a
    /// crafted stream and is dropped.
    kind: MessageKind,
    /// The author's nickname (from the first shard) — where a repair request
    /// for this group is addressed.
    author: Nickname,
    slots: BTreeMap<u32, String>,
    header: Option<Message>,
    bytes: usize,
    /// Last **progress** (a new slot filled) — duplicates don't refresh it,
    /// so resends can't keepalive a dead group past the idle TTL.
    last_seen: Instant,
    /// Birth instant, for the absolute lifetime ceiling.
    created_at: Instant,
    /// When we last asked the author to re-send missing shards, if ever.
    last_repair: Option<Instant>,
}

impl PartialGroup {
    fn is_complete(&self) -> bool {
        self.slots.len() as u64 == u64::from(self.total)
    }
}

/// Groups are keyed by `(author pubkey, group)` — the same security boundary
/// the message-log reassembly had: only the signer fills its own slots, so a
/// crafted shard carrying a victim's group id forms a separate,
/// never-completing set and can't inject a slice into the victim's body.
type GroupKey = (String, ShardGroup);

/// The dedicated buffer partial multipart bodies reassemble in — decoupled
/// from the message log so log eviction can never break reassembly, and
/// bounded by **byte budgets** (per group, per author, global) plus a
/// stale-group TTL instead of a shard-count cap. "Unbounded message size" is
/// achieved by budgeted, evictable buffering, never an unbounded collection.
#[derive(Debug, Default)]
pub struct ReassemblyStore {
    groups: HashMap<GroupKey, PartialGroup>,
    author_bytes: HashMap<String, usize>,
    total_bytes: usize,
}

impl ReassemblyStore {
    /// Buffer one shard; when it completes its group, synthesize and return
    /// the logical message and free the group's budget. The caller keys
    /// surfacing-once separately (`reassembled_groups`).
    pub(crate) fn ingest(&mut self, shard_msg: &Message, now: Instant) -> ShardIngest {
        let Some(shard) = shard_msg.shard.as_ref() else {
            return ShardIngest::Dropped;
        };
        let key: GroupKey = (shard_msg.pubkey.clone(), shard.group.clone());
        let charge = slot_charge(shard_msg.body.as_str().len());

        if let Some(group) = self.groups.get(&key) {
            // A signer contradicting its own group header (different total or
            // kind) is a crafted stream — drop the shard, keep the group.
            if group.total != shard.total || group.kind != shard_msg.kind {
                return ShardIngest::Dropped;
            }
            if group.slots.contains_key(&shard.idx) {
                // First-seen wins per idx. Deliberately NO liveness refresh: a
                // resent duplicate is not progress, and refreshing on it would
                // let an attacker keepalive a dead group forever.
                return ShardIngest::Dropped;
            }
            if group.bytes + charge > REASSEMBLY_GROUP_MAX_BYTES {
                self.evict(&key);
                tracing::warn!(
                    target: LOG_TARGET,
                    group = %key.1,
                    "partial shard group exceeded its byte budget; dropped"
                );
                return ShardIngest::Dropped;
            }
        }

        if !self.admit(&key, charge, now) {
            return ShardIngest::Dropped;
        }

        let group = self
            .groups
            .entry(key.clone())
            .or_insert_with(|| PartialGroup {
                total: shard.total,
                kind: shard_msg.kind.clone(),
                author: shard_msg.author.clone(),
                slots: BTreeMap::new(),
                header: None,
                bytes: 0,
                last_seen: now,
                created_at: now,
                last_repair: None,
            });
        group.last_seen = now;
        group.bytes += charge;
        group
            .slots
            .insert(shard.idx, shard_msg.body.as_str().to_owned());
        if shard.idx == 0 {
            group.header = Some(shard_msg.clone());
        }
        *self.author_bytes.entry(key.0.clone()).or_default() += charge;
        self.total_bytes += charge;

        if !group.is_complete() {
            return ShardIngest::Buffered;
        }
        let group_id = key.1.clone();
        let complete = self.evict(&key).expect("just-completed group is stored");
        match synthesize_logical(complete, &group_id) {
            Some(logical) => ShardIngest::Complete(Box::new(logical)),
            None => ShardIngest::Dropped,
        }
    }

    /// Reap partial groups that made no progress for [`REASSEMBLY_STALE_SECS`]
    /// or outlived [`REASSEMBLY_MAX_GROUP_LIFETIME_SECS`] outright (a trickle
    /// of one shard per idle window must not pin its bytes forever); returns
    /// how many were dropped (the prune tick warns when non-zero).
    pub(crate) fn sweep_stale(&mut self, now: Instant) -> usize {
        let stale: Vec<GroupKey> = self
            .groups
            .iter()
            .filter(|(_, group)| {
                now.duration_since(group.last_seen).as_secs() >= REASSEMBLY_STALE_SECS
                    || now.duration_since(group.created_at).as_secs()
                        >= REASSEMBLY_MAX_GROUP_LIFETIME_SECS
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in &stale {
            self.evict(key);
        }
        stale.len()
    }

    /// Make room for `charge` under the author and global budgets. The author
    /// budget evicts *that author's* stalest incomplete group (a hostile peer
    /// exhausts only itself); a global breach refuses the newcomer outright —
    /// evicting across authors would let a Sybil stream push out honest
    /// transfers.
    fn admit(&mut self, key: &GroupKey, charge: usize, _now: Instant) -> bool {
        while self.author_bytes.get(&key.0).copied().unwrap_or(0) + charge
            > REASSEMBLY_AUTHOR_BUDGET_BYTES
        {
            let Some(victim) = self.stalest_group_of(&key.0, key) else {
                tracing::warn!(
                    target: LOG_TARGET,
                    group = %key.1,
                    "author reassembly budget exhausted by one group; dropped"
                );
                return false;
            };
            self.evict(&victim);
            tracing::warn!(
                target: LOG_TARGET,
                group = %victim.1,
                "author reassembly budget breached; stalest partial group evicted"
            );
        }
        if self.total_bytes + charge > REASSEMBLY_TOTAL_BUDGET_BYTES {
            tracing::warn!(
                target: LOG_TARGET,
                group = %key.1,
                "global reassembly budget breached; incoming shard group dropped"
            );
            return false;
        }
        true
    }

    /// The author's least-recently-touched group other than `except`.
    fn stalest_group_of(&self, pubkey: &str, except: &GroupKey) -> Option<GroupKey> {
        self.groups
            .iter()
            .filter(|(key, _)| key.0 == pubkey && *key != except)
            .min_by_key(|(_, group)| group.last_seen)
            .map(|(key, _)| key.clone())
    }

    /// Accounting snapshot `(groups, total_bytes, max_author_bytes)` for the
    /// adversarial suite's budget tripwires.
    #[cfg(feature = "adversarial")]
    #[must_use]
    pub fn stats(&self) -> (usize, usize, usize) {
        (
            self.groups.len(),
            self.total_bytes,
            self.author_bytes.values().copied().max().unwrap_or(0),
        )
    }

    /// Drop `author`'s partial group and release its budget — the log-backed
    /// completion path calls this after finishing a group the store had only
    /// a fragment of (e.g. post-TTL, when dedup let just one healed shard
    /// re-ingest).
    pub(crate) fn forget(&mut self, pubkey: &str, group: &ShardGroup) {
        self.evict(&(pubkey.to_owned(), group.clone()));
    }

    /// The big (unlogged) groups that stalled long enough to ask their author
    /// for the missing shards, each at most once per
    /// [`REASSEMBLY_REPAIR_IDLE_SECS`] window. Small groups are excluded —
    /// their shards live in the message log and heal via anti-entropy.
    #[must_use]
    pub fn repair_tickets(&mut self, now: Instant) -> Vec<RepairTicket> {
        let mut tickets = Vec::new();
        for ((_, group_id), group) in &mut self.groups {
            if group.total <= LOGGED_SHARD_GROUP_MAX_TOTAL || group.is_complete() {
                continue;
            }
            let idle = now.duration_since(group.last_seen).as_secs();
            let asked_recently = group
                .last_repair
                .is_some_and(|at| now.duration_since(at).as_secs() < REASSEMBLY_REPAIR_IDLE_SECS);
            if idle < REASSEMBLY_REPAIR_IDLE_SECS || asked_recently {
                continue;
            }
            group.last_repair = Some(now);
            let missing: Vec<u32> = (0..group.total)
                .filter(|idx| !group.slots.contains_key(idx))
                .take(REASSEMBLY_REPAIR_MAX_IDXS)
                .collect();
            tickets.push(RepairTicket {
                author: group.author.clone(),
                group: group_id.clone(),
                missing,
            });
        }
        tickets
    }

    /// Remove a group and release its budget.
    fn evict(&mut self, key: &GroupKey) -> Option<PartialGroup> {
        let group = self.groups.remove(key)?;
        self.total_bytes = self.total_bytes.saturating_sub(group.bytes);
        if let Some(author) = self.author_bytes.get_mut(&key.0) {
            *author = author.saturating_sub(group.bytes);
            if *author == 0 {
                self.author_bytes.remove(&key.0);
            }
        }
        Some(group)
    }
}

/// Build the logical-message surfacing view from a completed group: shard 0's
/// envelope (kind/author/pubkey/mesh/ts), the concatenated body, the group as
/// `id`, and no `shard`/chain (the view is neither a wire message nor a chain
/// entry — the shards are). `None` only when the concatenated body fails
/// validation — fail closed rather than surface a malformed body.
fn synthesize_logical(group: PartialGroup, group_id: &ShardGroup) -> Option<Message> {
    let mut body = String::new();
    for slot in group.slots.values() {
        body.push_str(slot);
    }
    let body = MessageBody::new(body).ok()?;
    Some(synthesize_from(group.header?, body, group_id))
}

/// One stalled big group's repair ask: which author to query and which shard
/// indexes we still lack.
#[derive(Debug)]
pub struct RepairTicket {
    pub author: Nickname,
    pub group: ShardGroup,
    pub missing: Vec<u32>,
}

/// The sender-side re-serve source for big (unlogged) outbound groups: their
/// serialized, signed shard frames, kept so a receiver's `shard/repair`
/// request can be answered. Big groups skip the message log (one huge body
/// must not evict the anti-entropy history), so without this cache a lost
/// gossip frame would make the whole body unrecoverable. Byte-budgeted with
/// whole-group FIFO eviction — the bounded-everything discipline.
#[derive(Debug, Default)]
pub struct ShardCache {
    groups: VecDeque<CachedGroup>,
    bytes: usize,
}

#[derive(Debug)]
struct CachedGroup {
    group: ShardGroup,
    frames: Vec<bytes::Bytes>,
}

impl ShardCache {
    /// Cache one just-sent group's frames (indexed by shard `idx`), evicting
    /// the oldest groups past the byte budget. The newest group always stays.
    pub fn insert(&mut self, group: ShardGroup, frames: Vec<bytes::Bytes>) {
        self.bytes += frames.iter().map(bytes::Bytes::len).sum::<usize>();
        self.groups.push_back(CachedGroup { group, frames });
        while self.groups.len() > 1 && self.bytes > SHARD_CACHE_BUDGET_BYTES {
            if let Some(evicted) = self.groups.pop_front() {
                self.bytes = self
                    .bytes
                    .saturating_sub(evicted.frames.iter().map(bytes::Bytes::len).sum::<usize>());
            }
        }
    }

    /// The cached frames for `group` at the requested indexes (missing or
    /// out-of-range indexes are skipped; empty when the group was evicted).
    #[must_use]
    pub fn frames(&self, group: &ShardGroup, idxs: &[u32]) -> Vec<bytes::Bytes> {
        let Some(cached) = self.groups.iter().find(|entry| entry.group == *group) else {
            return Vec::new();
        };
        idxs.iter()
            .filter_map(|idx| cached.frames.get(*idx as usize).cloned())
            .collect()
    }
}

/// The shared synthesis step: `envelope` (a shard-0 frame) becomes the logical
/// view with the concatenated `body`, the group as `id`, and no `shard`/chain.
/// Also used by the message-log completion path (`EventLoopState::
/// reassemble_from_log`) so the two paths can't drift.
pub(crate) fn synthesize_from(
    mut envelope: Message,
    body: MessageBody,
    group_id: &ShardGroup,
) -> Message {
    envelope.id = MessageId::new(group_id.as_str()).expect("a shard group is a valid message id");
    envelope.body = body;
    envelope.shard = None;
    envelope.seq = None;
    envelope.prev = None;
    envelope.parents = Vec::new();
    envelope
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{ReassemblyStore, ShardIngest};
    use crate::protocol::{
        AppFrameParams, MeshId, Message, MessageBody, Nickname, Shard, ShardGroup,
    };
    use crate::util::consts::{
        REASSEMBLY_AUTHOR_BUDGET_BYTES, REASSEMBLY_GROUP_MAX_BYTES, REASSEMBLY_STALE_SECS,
        REASSEMBLY_TOTAL_BUDGET_BYTES,
    };

    fn group(uuid: &str) -> ShardGroup {
        ShardGroup::from_uuid_str(uuid).expect("valid group")
    }

    fn group_a() -> ShardGroup {
        group("550e8400-e29b-41d4-a716-446655440000")
    }

    /// A shard frame notionally signed by `pubkey`.
    fn shard_msg(body: &str, pubkey: &str, shard: Shard) -> Message {
        let mut message = Message::new_app(
            &MeshId::from("test"),
            &Nickname::from("author"),
            AppFrameParams {
                tag: crate::protocol::AppTag::from("app_msg"),
                to: None,
                corr: None,
                body: MessageBody::from(body),
            },
        );
        message.pubkey = pubkey.to_owned();
        message.shard = Some(shard);
        message
    }

    fn ingest(store: &mut ReassemblyStore, msg: &Message, now: Instant) -> ShardIngest {
        store.ingest(msg, now)
    }

    #[test]
    fn isolates_by_author() {
        // Two authors reuse the SAME group id. Each forms its own set; one
        // author's shard never fills another's slot, so a crafted cross-author
        // shard can't inject a slice into the victim's reassembled body.
        let now = Instant::now();
        let mut store = ReassemblyStore::default();
        assert!(matches!(
            ingest(
                &mut store,
                &shard_msg(
                    "alice-0",
                    "alice",
                    Shard {
                        group: group_a(),
                        idx: 0,
                        total: 2
                    }
                ),
                now
            ),
            ShardIngest::Buffered
        ));
        assert!(
            matches!(
                ingest(
                    &mut store,
                    &shard_msg(
                        "mallory-1",
                        "mallory",
                        Shard {
                            group: group_a(),
                            idx: 1,
                            total: 2
                        }
                    ),
                    now
                ),
                ShardIngest::Buffered
            ),
            "mallory's same-group shard forms a separate set, never completes alice's"
        );
        // Alice's own second shard completes with alice's bodies only.
        let ShardIngest::Complete(logical) = ingest(
            &mut store,
            &shard_msg(
                "alice-1",
                "alice",
                Shard {
                    group: group_a(),
                    idx: 1,
                    total: 2,
                },
            ),
            now,
        ) else {
            panic!("alice's set completes");
        };
        assert_eq!(logical.body.as_str(), "alice-0alice-1");
    }

    #[test]
    fn slots_by_idx_not_arrival() {
        let now = Instant::now();
        let mut store = ReassemblyStore::default();
        ingest(
            &mut store,
            &shard_msg(
                "b",
                "a",
                Shard {
                    group: group_a(),
                    idx: 1,
                    total: 3,
                },
            ),
            now,
        );
        ingest(
            &mut store,
            &shard_msg(
                "a",
                "a",
                Shard {
                    group: group_a(),
                    idx: 0,
                    total: 3,
                },
            ),
            now,
        );
        let ShardIngest::Complete(logical) = ingest(
            &mut store,
            &shard_msg(
                "c",
                "a",
                Shard {
                    group: group_a(),
                    idx: 2,
                    total: 3,
                },
            ),
            now,
        ) else {
            panic!("third shard completes the group");
        };
        assert_eq!(logical.body.as_str(), "abc", "slotted by idx, not arrival");
        assert_eq!(logical.id.as_str(), group_a().as_str(), "id is the group");
        assert!(logical.shard.is_none(), "the view carries no shard header");
    }

    #[test]
    fn header_mismatch_is_dropped() {
        let now = Instant::now();
        let mut store = ReassemblyStore::default();
        ingest(
            &mut store,
            &shard_msg(
                "x",
                "a",
                Shard {
                    group: group_a(),
                    idx: 0,
                    total: 3,
                },
            ),
            now,
        );
        // Same signer contradicting its own advertised total: crafted stream.
        assert!(matches!(
            ingest(
                &mut store,
                &shard_msg(
                    "y",
                    "a",
                    Shard {
                        group: group_a(),
                        idx: 1,
                        total: 4
                    }
                ),
                now
            ),
            ShardIngest::Dropped
        ));
        // A different kind under the same group is equally crafted.
        let mut wrong_kind = shard_msg(
            "z",
            "a",
            Shard {
                group: group_a(),
                idx: 1,
                total: 3,
            },
        );
        wrong_kind.kind = crate::protocol::MessageKind::Presence {
            subtype: crate::protocol::PresenceSubtype::Joined,
        };
        assert!(matches!(
            ingest(&mut store, &wrong_kind, now),
            ShardIngest::Dropped
        ));
    }

    #[test]
    fn duplicate_idx_is_dropped_first_seen_wins() {
        let now = Instant::now();
        let mut store = ReassemblyStore::default();
        ingest(
            &mut store,
            &shard_msg(
                "first",
                "a",
                Shard {
                    group: group_a(),
                    idx: 0,
                    total: 2,
                },
            ),
            now,
        );
        assert!(matches!(
            ingest(
                &mut store,
                &shard_msg(
                    "second",
                    "a",
                    Shard {
                        group: group_a(),
                        idx: 0,
                        total: 2
                    }
                ),
                now
            ),
            ShardIngest::Dropped
        ));
        let ShardIngest::Complete(logical) = ingest(
            &mut store,
            &shard_msg(
                "!",
                "a",
                Shard {
                    group: group_a(),
                    idx: 1,
                    total: 2,
                },
            ),
            now,
        ) else {
            panic!("completes");
        };
        assert_eq!(logical.body.as_str(), "first!", "first-seen wins per idx");
    }

    #[test]
    fn group_budget_breach_drops_the_group() {
        let now = Instant::now();
        let mut store = ReassemblyStore::default();
        let big = "x".repeat(REASSEMBLY_GROUP_MAX_BYTES / 2 + 1);
        ingest(
            &mut store,
            &shard_msg(
                &big,
                "a",
                Shard {
                    group: group_a(),
                    idx: 0,
                    total: 3,
                },
            ),
            now,
        );
        // The second shard pushes the group past its byte ceiling: the whole
        // group is dropped, and its budget released.
        assert!(matches!(
            ingest(
                &mut store,
                &shard_msg(
                    &big,
                    "a",
                    Shard {
                        group: group_a(),
                        idx: 1,
                        total: 3
                    }
                ),
                now
            ),
            ShardIngest::Dropped
        ));
        assert_eq!(store.total_bytes, 0, "dropped group released its budget");
        assert!(store.groups.is_empty());
    }

    #[test]
    fn author_budget_evicts_the_stalest_group() {
        let start = Instant::now();
        let mut store = ReassemblyStore::default();
        // Sized so two groups fit the author budget but three overflow it.
        let chunk = "x".repeat(REASSEMBLY_GROUP_MAX_BYTES / 2 + 64);
        // Two half-budget groups from one author fill its allowance...
        let stale = group("00000000-0000-4000-8000-000000000001");
        let fresh = group("00000000-0000-4000-8000-000000000002");
        ingest(
            &mut store,
            &shard_msg(
                &chunk,
                "a",
                Shard {
                    group: stale.clone(),
                    idx: 0,
                    total: 2,
                },
            ),
            start,
        );
        ingest(
            &mut store,
            &shard_msg(
                &chunk,
                "a",
                Shard {
                    group: fresh.clone(),
                    idx: 0,
                    total: 2,
                },
            ),
            start + Duration::from_secs(1),
        );
        // ...so a third group evicts the stalest (the first), not the freshest.
        let newcomer = group("00000000-0000-4000-8000-000000000003");
        assert!(matches!(
            ingest(
                &mut store,
                &shard_msg(
                    &chunk,
                    "a",
                    Shard {
                        group: newcomer.clone(),
                        idx: 0,
                        total: 2
                    }
                ),
                start + Duration::from_secs(2)
            ),
            ShardIngest::Buffered
        ));
        assert!(
            !store.groups.contains_key(&("a".to_owned(), stale)),
            "stalest group evicted"
        );
        assert!(store.groups.contains_key(&("a".to_owned(), fresh)));
        assert!(store.groups.contains_key(&("a".to_owned(), newcomer)));
    }

    #[test]
    fn global_budget_refuses_the_newcomer() {
        let now = Instant::now();
        let mut store = ReassemblyStore::default();
        let chunk = "x".repeat(REASSEMBLY_GROUP_MAX_BYTES / 2);
        // Sybil authors each stay under their own budget until the global
        // backstop bites; the newcomer is refused rather than evicting an
        // honest author's transfer.
        let mut fitted = 0usize;
        for author in 0..64u32 {
            let sybil_group = group(&format!("00000000-0000-4000-8000-0000000000{author:02x}"));
            match ingest(
                &mut store,
                &shard_msg(
                    &chunk,
                    &format!("sybil-{author}"),
                    Shard {
                        group: sybil_group,
                        idx: 0,
                        total: 2,
                    },
                ),
                now,
            ) {
                ShardIngest::Buffered => fitted += 1,
                ShardIngest::Dropped => break,
                ShardIngest::Complete(_) => panic!("nothing completes here"),
            }
        }
        assert!(
            fitted <= REASSEMBLY_TOTAL_BUDGET_BYTES / (REASSEMBLY_GROUP_MAX_BYTES / 2),
            "global backstop caps buffered bytes across Sybil authors"
        );
        assert!(store.total_bytes <= REASSEMBLY_TOTAL_BUDGET_BYTES);
    }

    #[test]
    fn stale_groups_are_swept_and_release_budget() {
        let start = Instant::now();
        let mut store = ReassemblyStore::default();
        ingest(
            &mut store,
            &shard_msg(
                "x",
                "a",
                Shard {
                    group: group_a(),
                    idx: 0,
                    total: 3,
                },
            ),
            start,
        );
        // Progress (a NEW slot) refreshes the group's clock...
        let touched = start + Duration::from_secs(REASSEMBLY_STALE_SECS - 1);
        assert!(matches!(
            ingest(
                &mut store,
                &shard_msg(
                    "y",
                    "a",
                    Shard {
                        group: group_a(),
                        idx: 1,
                        total: 3
                    }
                ),
                touched
            ),
            ShardIngest::Buffered
        ));
        // ...but a duplicate slot does NOT — no attacker keepalive via resends.
        let dup_at = touched + Duration::from_secs(REASSEMBLY_STALE_SECS - 1);
        assert!(matches!(
            ingest(
                &mut store,
                &shard_msg(
                    "x",
                    "a",
                    Shard {
                        group: group_a(),
                        idx: 0,
                        total: 3
                    }
                ),
                dup_at
            ),
            ShardIngest::Dropped
        ));
        assert_eq!(
            store.sweep_stale(touched + Duration::from_secs(REASSEMBLY_STALE_SECS - 1)),
            0,
            "a progressing group survives the sweep"
        );
        assert_eq!(
            store.sweep_stale(touched + Duration::from_secs(REASSEMBLY_STALE_SECS)),
            1,
            "a group with no new slots is reaped at the idle TTL (the dup did not refresh)"
        );
        assert_eq!(store.total_bytes, 0);
        assert!(store.author_bytes.is_empty());
    }

    #[test]
    fn a_trickling_group_is_reaped_at_the_lifetime_ceiling() {
        use crate::util::consts::REASSEMBLY_MAX_GROUP_LIFETIME_SECS;
        let start = Instant::now();
        let mut store = ReassemblyStore::default();
        // One new shard per idle window keeps the group alive under the idle
        // TTL alone — feed progress right up to the lifetime ceiling.
        let total = 64u32;
        let mut at = start;
        let mut idx = 0u32;
        while at.duration_since(start).as_secs() < REASSEMBLY_MAX_GROUP_LIFETIME_SECS {
            ingest(
                &mut store,
                &shard_msg(
                    "x",
                    "a",
                    Shard {
                        group: group_a(),
                        idx: idx % total,
                        total,
                    },
                ),
                at,
            );
            idx += 1;
            at += Duration::from_secs(REASSEMBLY_STALE_SECS - 1);
        }
        assert_eq!(store.groups.len(), 1, "the trickle kept the group alive");
        assert_eq!(
            store.sweep_stale(start + Duration::from_secs(REASSEMBLY_MAX_GROUP_LIFETIME_SECS)),
            1,
            "the lifetime ceiling reaps it regardless of progress"
        );
        assert_eq!(store.total_bytes, 0);
    }

    #[test]
    fn completion_releases_the_budget() {
        let now = Instant::now();
        let mut store = ReassemblyStore::default();
        ingest(
            &mut store,
            &shard_msg(
                "a",
                "a",
                Shard {
                    group: group_a(),
                    idx: 0,
                    total: 2,
                },
            ),
            now,
        );
        assert!(store.total_bytes > 0);
        assert!(matches!(
            ingest(
                &mut store,
                &shard_msg(
                    "b",
                    "a",
                    Shard {
                        group: group_a(),
                        idx: 1,
                        total: 2
                    }
                ),
                now
            ),
            ShardIngest::Complete(_)
        ));
        assert_eq!(store.total_bytes, 0);
        assert!(store.author_bytes.is_empty());
        assert!(store.groups.is_empty());
    }

    #[test]
    fn repair_tickets_target_stalled_big_groups_once_per_window() {
        use crate::util::consts::{LOGGED_SHARD_GROUP_MAX_TOTAL, REASSEMBLY_REPAIR_IDLE_SECS};
        let start = Instant::now();
        let mut store = ReassemblyStore::default();
        // A small group (logged, heals via anti-entropy) and a big one.
        let small = group("00000000-0000-4000-8000-00000000000a");
        let big = group("00000000-0000-4000-8000-00000000000b");
        ingest(
            &mut store,
            &shard_msg(
                "s",
                "a",
                Shard {
                    group: small,
                    idx: 0,
                    total: 2,
                },
            ),
            start,
        );
        ingest(
            &mut store,
            &shard_msg(
                "b",
                "a",
                Shard {
                    group: big.clone(),
                    idx: 0,
                    total: LOGGED_SHARD_GROUP_MAX_TOTAL + 4,
                },
            ),
            start,
        );
        ingest(
            &mut store,
            &shard_msg(
                "b",
                "a",
                Shard {
                    group: big.clone(),
                    idx: 2,
                    total: LOGGED_SHARD_GROUP_MAX_TOTAL + 4,
                },
            ),
            start,
        );

        // Not stalled yet — no tickets.
        assert!(store.repair_tickets(start).is_empty());

        // Stalled: only the big group is ticketed, with exactly its gaps.
        let stalled = start + Duration::from_secs(REASSEMBLY_REPAIR_IDLE_SECS);
        let tickets = store.repair_tickets(stalled);
        assert_eq!(tickets.len(), 1, "small groups heal via anti-entropy");
        assert_eq!(tickets[0].group, big);
        assert_eq!(tickets[0].author.as_str(), "author");
        assert!(!tickets[0].missing.contains(&0));
        assert!(!tickets[0].missing.contains(&2));
        assert!(tickets[0].missing.contains(&1));

        // Asked once per window — the immediate re-check yields nothing.
        assert!(store.repair_tickets(stalled).is_empty());
        // The next window asks again.
        let next = stalled + Duration::from_secs(REASSEMBLY_REPAIR_IDLE_SECS);
        assert_eq!(store.repair_tickets(next).len(), 1);
    }

    #[test]
    fn shard_cache_serves_requested_frames_and_evicts_whole_groups() {
        use crate::util::consts::SHARD_CACHE_BUDGET_BYTES;
        let mut cache = super::ShardCache::default();
        let first = group("00000000-0000-4000-8000-0000000000c1");
        let frame = |tag: u8, len: usize| bytes::Bytes::from(vec![tag; len]);
        cache.insert(first.clone(), vec![frame(1, 10), frame(2, 10)]);
        let served = cache.frames(&first, &[1, 7]);
        assert_eq!(served.len(), 1, "out-of-range idx skipped");
        assert_eq!(served[0][0], 2);

        // A budget-filling newcomer evicts the oldest group whole.
        let second = group("00000000-0000-4000-8000-0000000000c2");
        cache.insert(second.clone(), vec![frame(3, SHARD_CACHE_BUDGET_BYTES)]);
        assert!(cache.frames(&first, &[0]).is_empty(), "old group evicted");
        assert_eq!(
            cache.frames(&second, &[0]).len(),
            1,
            "the newest group always stays"
        );
    }

    #[test]
    fn author_budget_never_exceeded_under_random_streams() {
        // Property-ish sweep: many interleaved groups from two authors, with
        // crafted totals; whatever the interleaving, the store's accounting
        // never exceeds the budgets.
        let now = Instant::now();
        let mut store = ReassemblyStore::default();
        let chunk = "x".repeat(4096);
        for round in 0..2048u32 {
            let author = if round % 3 == 0 { "a" } else { "b" };
            let group = group(&format!(
                "00000000-0000-4000-8000-0000000{:05x}",
                round % 37
            ));
            let _ = ingest(
                &mut store,
                &shard_msg(
                    &chunk,
                    author,
                    Shard {
                        group,
                        idx: round % 5,
                        total: u32::MAX,
                    },
                ),
                now,
            );
            assert!(store.total_bytes <= REASSEMBLY_TOTAL_BUDGET_BYTES);
            assert!(
                store
                    .author_bytes
                    .values()
                    .all(|bytes| *bytes <= REASSEMBLY_AUTHOR_BUDGET_BYTES)
            );
        }
    }
}
