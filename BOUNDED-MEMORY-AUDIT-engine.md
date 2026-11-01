# Bounded memory audit: habilis-network engine

Scope: daemon-shipping crates of `habilis-network` (read-only).
Base: pinned commit `f7cd9bbc`. Line numbers below are for `f7cd9bbc`, except where a row says it was not re-verified. The first draft read a dirty worktree, and many file paths changed at the pin. Rows that were not re-checked at the pin are marked `(not re-verified)`.
Status: partial. Worst-case bytes are estimates from struct shapes, not measured.
Sizes: one wire frame is at most 3,840 B (`MAX_MESSAGE_SIZE`). One stored frame is at most 3,920 B (3,840 B plus about 80 B overhead). Every row uses this value.

## Constants checked

- `MAX_MESSAGE_SIZE` = 3840 B (`habilis-network-util/src/consts.rs:45`).
- `MAX_LOGICAL_BODY_BYTES` = 64 MiB (`consts.rs:71`).
- `REASSEMBLY_GROUP_MAX_BYTES` = 128 MiB (`consts.rs:90`). This is two bodies, not one.
- `REASSEMBLY_AUTHOR_BUDGET_BYTES` = 192 MiB (`consts.rs:95`).
- `REASSEMBLY_TOTAL_BUDGET_BYTES` = 384 MiB (`consts.rs:100`). This equals 6 x 64 MiB.
- `SHARD_CACHE_BUDGET_BYTES` = 128 MiB (`consts.rs:159`).

## Bounded types

- `BoundedFifoSet`: drops the oldest item.
- `BoundedQueue`: refuses the newest item.
- `MessageLog`: evicts the item with the smallest eviction key, not the oldest. Do not call it FIFO.
- `Cooldown`, `RefusalBackoff`, `EvictionBackoff`, `ServeBudget`: prune by time window on each insert.

## Table: unbounded rows first, by worst-case bytes

| # | file:line | type | growth driver | bound | worst case | when full | proposed bound and drop rule |
|---|---|---|---|---|---|---|---|
| 1 | `habilis-network-doc/src/mesh_doc.rs:509` (`MeshDoc.frames`) | `HashMap<ChangeHash, Verified>` | every applied change; only inserts (`mesh_doc.rs:508-509`). Largest real leak: about 3 meta changes per minute gives about 1.58M frames per year per document | NONE | per year: 1,576,800 x 3,920 B ≈ 6.2 GB per document | grows | snapshot, then drop frames covered by a checkpoint |
| 2 | `habilis-network-doc/src/lib.rs:133` (`MeshDoc.doc`) | Automerge doc | full op history | NONE | not measured | grows | periodic save and compaction |
| 4 | `habilis-network/src/transport/pool/unicast_pool.rs:256` (`addrs`) | `Mutex<HashMap<EndpointId, EndpointAddr>>` | each address note. Graceful leave removes the entry: `forget` (`unicast_pool.rs:720`), called from `lifecycle/presence_event.rs:55`. Remaining risk: an address noted at `probe.rs:998` or `:1092` (not re-verified) for an endpoint that no `forget` reaches | NONE in count; bounded per peer on graceful leave only | per leaked endpoint ≈ 300 B est.; grows only with endpoints that never leave gracefully | grows | cap with FIFO at `KNOWN_ENDPOINTS_CAP`; drop on silence timeout too |
| 5 | `habilis-network/src/daemon/state.rs:592` (`address_book`) | iroh `MemoryLookup` | each first sighting; only `beacon_arm.rs` removes | NONE (unverified) | ~300 MB per 1M | unknown | forget with row 4 |
| 6 | `habilis-network/src/membership/event.rs:177` (`json_sink`) | unbounded channel | every event line | NONE by design | 1M x ~300 B ≈ 300 MB | grows if consumer stops | bounded; drop app-message lines first |
| 7 | `habilis-network/src/gossip/split.rs:187` (`control_tx`) | unbounded channel | topic events from peer churn | NONE | ~200 MB | grows if stalled | coalesce per peer |
| 8 | `habilis-network/src/transport/graft_backoff.rs:48` (`by_peer`) (not re-verified at pin) | `HashMap<EndpointId, Entry>` | each refused neighbor request. Attack path: any member can mint keys at no cost | NONE in time | 1M x ~110 B ≈ 110 MB | grows | count cap 256, drop the oldest |
| 9 | `habilis-network/src/daemon/state.rs:239, 263` (`direct`, `path_kinds`) | `HashMap` | each probed endpoint; rebind keeps the old endpoint | NONE on rebind path | 1M x 2 x ~50 B ≈ 100 MB | grows | remove old endpoint entries on bind |
| 10 | `habilis-network/src/transport/admission/inner.rs:54` (`closed_on_rung`) | `HashMap<EndpointId, Instant>` | each peer closed by the recursion rule (`signal_admission.rs:157-158`) | `RUNG_CLOSE_MEMORY` 15 s; pruned by `retain` on each sync (`inner.rs:196-197`) | keys in the 15 s window | n/a | keep |
| 11 | `habilis-network/src/daemon/event_loop.rs:216` (`direct_tx`) | unbounded channel | each probe verdict | NONE | ~80 MB if stalled | grows | bounded; coalesce per peer |
| 12 | `habilis-network/src/daemon/event_loop.rs:222` (`path_tx`) | unbounded channel | each path change | NONE | ~64 MB if stalled | grows | coalesce to latest per endpoint |
| 13 | `event_loop.rs:240` (`rung_report_tx`); `beacon/mod.rs:1558` (`reports`) | unbounded channel | each rung probe | NONE | ~64 MB if stalled | grows | bounded; coalesce by rung |
| 14 | `habilis-network-doc/src/lib.rs:136` (`applied`) | `HashSet<ChangeHash>` | each applied change | NONE | 1M x ~50 B ≈ 50 MB | grows | drop with frames on compaction |
| 15 | `habilis-network-doc/src/lib.rs:162` (`dropped`) | `Vec<DedupKey>` | refused or evicted orphans until drained | NONE by type | ~16 MB if never drained | grows | cap length; drop oldest |
| 16 | `habilis-network/src/gossip/lan.rs:51` (`LanGrafts.asked`) | `HashMap` | each LAN graft, max 8 per minute | rate only | ~2.9 MB per day without Gone events | grows | TTL 10 min; cap 256 |
| 18 | `habilis-network-iroh-multihop-transport/src/topology.rs:117, 122` (`vectors`, `tombstones`) | `HashMap` | each signed vector from a new origin | time only, 45 s | rate x 45 s x 3.8 KB; 1000 origins/s ≈ 171 MB | time-expires only | count cap 1024, drop oldest |
| 19 | `habilis-network-iroh-multihop-transport/src/underlay.rs:279, 427, 431` (writers, unresolved, heard hops) | `Mutex<HashMap>`, `HashSet`+`VecDeque` | per hop, per writer | `MAX_WRITERS` 64; `UNRESOLVED_SEEN_CAP` 128; `HEARD_HOPS_CAP` 1024 | forward queue 32 MiB | writers refuse; heard drops oldest | keep |
| 20 | `habilis-network/src/transport/pool.rs:87, 100` (`conns`, `dial_failures`) | `Mutex<HashMap>`, `Mutex<Cooldown>` | each dialed peer | idle sweep; cooldown 10 s; no count cap | live connections x idle time | idle sweep | count cap |
| 21 | `habilis-network/src/daemon/state.rs:260, 267` (`path_watchers`, `route_dialed`) | `HashMap` | each watched path or dialed route | removed on replace and forget | roster-bound | removed on forget | keep |
| 22 | `habilis-network/src/beacon/mod.rs:923` (`tasks`) | `Vec<JoinHandle>` | each beacon task | `retain !is_finished` | live tasks only | finished tasks dropped | keep |
| 23 | `habilis-network/src/blob/store/blob_store.rs:131` (`evict_to_budget`) | `HashMap<BlobDigest, BlobEntry>` | each produced blob | `MAX_BLOB_STORE_BYTES` 4 GiB (`evict_to_budget`, `blob_store.rs:131-146`) | 4 GiB on disk | removes the least recently registered blob and deletes its file (`remove`, `blob_store.rs:146-150`), even when a peer or user still refers to it. Files can vanish silently | refuse the new blob when over budget; never evict a referenced blob (user work rule) |
| 24 | `habilis-network-chunks/src/lib.rs:181` (`ChunkMap.leaves`) | `Vec<ChunkHash>` | each blob | `MAX_BLOB_BYTES` 2 GiB / 64 KiB = 32,768 leaves | 1 MiB per blob | n/a | keep |

### Bounded rows (cap and drop rule known)

| file:line | type | bound | worst case | when full |
|---|---|---|---|---|
| `habilis-network-doc/src/lib.rs:147` (`pending`) | `HashMap` | `DOC_PENDING_TOTAL_MAX` 512; `DOC_PENDING_AUTHOR_MAX` 128 | ~4 MB | refuse or evict |
| `habilis-network/src/daemon/state.rs:641` (`pending_outbound`) | `BoundedQueue` | `PENDING_OUTBOUND_CAP` 1024 | ~4.7 MB | refuses newest |
| `habilis-network/src/daemon/message_log.rs:76` (`message_log`) | `VecDeque` + maps | `MESSAGE_LOG_SIZE` 1000 | ~4.3 MB | evicts smallest key |
| `habilis-network-protocol/src/reassembly.rs:338` (`ShardCache`) | `VecDeque` | `SHARD_CACHE_BUDGET_BYTES` 128 MiB | 128 MiB | drops oldest group |
| `habilis-network-protocol/src/reassembly.rs:88-89` (`ReassemblyStore`) | `HashMap` | 384 MiB total; 192 MiB per author; 128 MiB per group | 384 MiB | refuse shard or evict author's stalest |
| `habilis-network/src/daemon/state.rs:274` (`known_endpoints`) | `BoundedFifoSet` | 64 | ~4 KB | drops oldest |
| `habilis-network/src/daemon/state.rs:634` (`seen`) | `BoundedFifoSet` | `SEEN_IDS_CAP` 2000 | ~128 KB | drops oldest |
| `habilis-network/src/daemon/state.rs:373, 386, 399` (`quiet`, `left`, `left_keys`) | `BoundedFifoSet` | `QUIET_CAP` 1024 | ~200 KB each | drops oldest |
| `habilis-network/src/daemon/state.rs:659` (`reassembled_groups`) | `BoundedFifoSet` | `MESSAGE_LOG_SIZE` | ~100 KB | drops oldest |
| `habilis-network/src/daemon/state.rs:83-84` (`ParkedLinkStates`) | `HashMap` | 256 | ~1 MB | refuses new |
| `habilis-network/src/gossip/split.rs:186` (mesh data channel) | `mpsc` | `MESH_TOPIC_EVENTS_CAP` 2048 | ~8 MB | drops newest |
| `habilis-network/src/transport/outbox.rs:31` (`ResendOutbox`) | `mpsc` | `RESEND_OUTBOX_CAP` 256 | not measured | refuses new |
| `habilis-network-protocol/src/reassembly.rs` (unicast inbox) | `mpsc` | `UNICAST_INBOX_CAP` 256 | ~1 MB | drops newest |
| `habilis-network/src/daemon/state.rs:309, 311` (`lane_wanted`, `regraft_at`) | `HashMap` | 60 s and 1 s windows | rate-bound | pruned on insert |
| `habilis-network/src/daemon/state.rs:316, 319` (`digest_serves`, `digest_unreadable`) | `Cooldown` | 5 s and 10 min windows | rate-bound | pruned on note |
| `habilis-network/src/transport/admission.rs:86, 90` (`RefusalBackoff`) | `HashMap` | 5 min window | rate-bound | pruned on note |
| `habilis-network/src/transport/ceiling.rs:54` (`EvictionBackoff`) | `HashMap` | 60 s window | rate-bound | pruned on note |
| `habilis-network/src/daemon/state.rs:552` (`FastRounds.ahead`) | `HashMap<_, Vec>` | 16 per channel; 60 s TTL | ~128 KB | refuses past 16 |
| `habilis-network/src/daemon/state.rs:227` (`linked_endpoints`) | `HashSet` | HyParView active view 32 | 32 entries | enforced by HyParView |

## Added rows: spawn sites, receive path, probe and pool, lookup, join, WebRTC, beacon, doc gate, graph

Sizes marked `est.` come from struct shape, not measurement. `unverified` means the code was not confirmed.

| collection and file:line | type | growth driver | bound | worst case | when full | proposed bound and drop rule |
|---|---|---|---|---|---|---|
| `event_loop.rs:216` `direct_tx` | unbounded channel | one probe verdict per probe | none; producers bounded by probes in flight | est. 40 B per message | never full | bounded channel sized to probes in flight |
| `event_loop.rs:222` `path_tx` | unbounded channel | path change per watched conn | none; producers bounded by watched conns | est. 50 B per message | never full | bounded channel sized to watched conns |
| `event_loop.rs:226` `flush_failed_tx` | unbounded channel | failed flush, one per `watch_flush` task | producers come from `pending_outbound.take()` (`gossip/recv.rs:420`), capped at `PENDING_OUTBOUND_CAP` 1024 | 1024 x 3,920 B ≈ 4.0 MB | never full | bounded 1024; drop and count when full |
| `transport/ipc.rs:147` `handle_connection` task | task | each local client connection | none on count | unverified | n/a | cap concurrent connections |
| `daemon/state.rs:347` `peers` | set | each distinct nickname heard | time only: `ALIVE_TIMEOUT` 90 s | N x ~66 B | grows within 90 s window | cap at roster size |
| `daemon/state.rs:360` `peer_endpoints` | map | each peer | pruned with `peers` | N x ~250 B est. | n/a | keep |
| `daemon/state.rs:239` `direct` | map | each probed or proven endpoint | pruned only in `forget_peer_endpoint`; not on `NeighborDown` | N x ~50 B | none | remove on `NeighborDown`; unverified for unnamed endpoints |
| `daemon/state.rs:469` `rival_holders` | map | signer keys with rival flag | removed on flag clear and roster retain; no count cap | N x ~72 B; keys free to mint | none | cap with FIFO; unverified whether non-roster signers reach it |
| `daemon/state.rs:74` `ParkedLinkStates` | map | link vectors for authors with no endpoint yet | count cap 256 | up to 16 MB (64 KB per cell, unverified) | refused at cap | cap bytes, not only count |
| `gossip/split.rs:187` `control_tx` | unbounded channel | NeighborUp/Down and default-route items | none | est. 100 B per item | never drops | bound it; drop oldest with a counter |
| `gossip/recv.rs:422` `watch_flush` task | task | one per flushed frame | `PENDING_OUTBOUND_CAP` 1024 | 1024 x 3,920 B ≈ 4.0 MB | returns frame on failure | bound by dial timeout (unverified) |
| `transport/probe.rs` nudge tasks | task | one per WebRTC rider per alive tick | none on count | small per task | n/a | rate-limit per peer with `Cooldown` |
| `transport/pool.rs:117` `addrs` | map | each address note | removed on `forget`; no count cap | N x ~280 B est. | none | cap at roster size |
| `transport/pool.rs:87` `conns` | map | each dialed or accepted connection | idle sweep; ceiling enforcement unverified | N x conn handle | n/a | explicit cap from `MAX_DIRECT_PEERS` |
| `lookup/mdns.rs:66` `known` | map | each distinct LAN endpoint | removed only on `Gone` | N x ~250 B est. | none | expire without `Gone` |
| iroh `MemoryLookup` (`daemon/state.rs:592,853`) | map inside iroh | every address added | iroh eviction unverified (no vendored source) | unverified | unverified | wrap with a cap if iroh does not evict |
| `membership/join.rs:149` inbound channel | channel | inbound `msg` frames | `INBOUND_CAP` 256 | 256 x 1,408 B ≈ 0.36 MB | drop policy unverified | keep |
| `membership/event.rs:178` `json_sink` | unbounded channel | every node event | none | est. 200 B per event | never drops | bounded; daemon use unverified (tests only in repo) |
| `registry.rs:55` WebRTC `slots` | map | one per attached session | `MAX_DIRECT_PEERS` 64 (`transport/webrtc.rs:678`); enforcement unverified | 64 x per-session size (unverified) | `reserve` refused | verify ceiling check |
| `admission/inner.rs:54` `closed_on_rung` | map | each closed peer | retained by `RUNG_CLOSE_MEMORY` 15 s (`inner.rs:196-197`) | keys in window | n/a | keep |
| `beacon/mod.rs` `RungWatch.tasks` | Vec | one per probe | live probes only | live probes only | n/a | keep |
| `beacon/mod.rs:1761` `arrivals` | channel(32) | arrivals | bounded 32 | 32 x 32 B | drop policy unverified | keep |
| `habilis-network-doc/src/lib.rs:147` `pending` | map | orphan frames | `DOC_PENDING_TOTAL_MAX` 512; `DOC_PENDING_AUTHOR_MAX` 128 | 512 x 3.9 KB ≈ 2 MB per doc | victim evicted | keep |
| `habilis-network-doc/src/lib.rs:162` `dropped` | Vec | orphans dropped | drained by `take_dropped` | per-ingest count | n/a | keep; relies on caller draining |
| `SelfWriteGate` per-peer map (`lib.rs:101-108`) | automerge map | one key per nickname | no cap; no removal on leave in crate | ~100 B per nick est. plus history | n/a | prune on leave (caller unverified) |
| `topology.rs:116` `vectors` (not re-verified) | map | each origin's freshest link vector. Attack path: any member can mint new origin keys at no cost | expiry after 45 s (`topology.rs:23`) | N origins x up to 64 KB (unverified) | refused or expired | count cap, drop the oldest |
| `topology.rs:122` `tombstones` | map | removed origins | retained by max age | N x ~24 B | n/a | keep |
| `graph.rs:18` `Graph` | struct | built per call | transient | O(V+E) per call | n/a | keep |
| `selector.rs:44` `select` | per call | path selection | no persistent state | 0 | n/a | keep |

## Documents A and B

**Frame size.** A stored frame is at most 3,840 B plus about 80 B overhead, so about 3,920 B worst case. Typical size is unverified.

**Document A: `MeshDoc` frame storage.** `applied` (`lib.rs:136`) and `frames` (`lib.rs:140`) each take one insert per applied change. Neither has a remove path in the crate.
- Frames: N x 3,920 B worst case.
- Applied hashes: N x about 40 B (est.).
- Per document at N = 1,000,000: about 3.96 GB. Meta and state together: about 7.9 GB, before Automerge history.

**Document B: Automerge history.** Not compacted. A grep of `crates/` finds no call to `save_incremental`, `save`, `save_after`, `compact`, `load`, `load_incremental`, or `autosave`. The crate has no persistence. Full history stays in memory for the process lifetime. Automerge cost per change is unverified.

## EventLoopState rows (`daemon/state/event_loop_state.rs` at `f7cd9bbc`)

| collection | type | growth driver | bound | worst case | when full | note |
|---|---|---|---|---|---|---|
| `author_seqs` (`:703`), `forked` (`:706`), `by_hash` (`:712`), `dag_heads` (`:713`) | maps and sets | each loggable chained message | bounded only because `push_to_log` (`gossip/recv.rs:1071`, forget path at `recv.rs:1030-1037`) calls `forget_msg_seq` and `forget_hash` for each evicted message (`event_loop_state.rs:1534`, `:1549`) | about 1,000 x 80 B ≈ 80 KB, tied to `MESSAGE_LOG_SIZE` | follows log eviction | dependency: removing or changing `push_to_log`'s forget calls makes these grow without bound |
| `last_seen` (`:350`), `quiet_since` (`:380`), `left_at` (`:388`), `surfaced` (`:408`) | map and set | each peer heard, or evicted or departed | pruned by `heartbeat.rs:50-101` | bounded by peer count | n/a | keep |
| `probes`, `pongs` (`:775-777`) | maps | each probe or pong | not re-verified at pin | unverified | unverified | verify before relying on it |
| `SelfWriteGate` peer keys (`habilis-network-doc`, `lib.rs:101-108` in the first draft) | automerge map keys | one key per nickname | nothing prunes them | ~100 B per nick est. | n/a | prune on leave (caller unverified) |

## Open items

- Rows 3, 6, 7, 11, 12, 13 grow only while a consumer stalls. The proof needs a test that stalls the consumer.
- Row 9: no test covers the rebind path.
- Row 18: no count cap. The proof needs a test that feeds 10x the planned cap of origins.
- Row 1 and row 2: the largest real growth path. The proof needs a snapshot design.
- Not read: the ~55 spawn sites in `event_loop`, `gossip/recv`, `transport/probe`, `transport/pool`; `lookup/*`; `membership/join.rs`; webrtc transports; beacon internals beyond the rung and arrival channels; the `SelfWriteGate` map in `habilis-network-doc`; automerge internals; `habilis-network-iroh-multihop-transport/src/graph.rs` and the selector.
- Row 5 depends on iroh internals that were not read.
- Idle counters (uncommitted) were not touched.
