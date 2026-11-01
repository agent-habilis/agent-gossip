# Bounded memory audit: habilis-network engine

Scope: daemon-shipping crates of `habilis-network` (read-only).
Status: partial. Worst-case bytes are estimates from struct shapes, not measured.
Assumed sizes: key 32 B, `Instant` 16 B, wire frame up to 3840 B, `Verified` about 0.8 KB extra.

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
| 1 | `habilis-network-doc/src/lib.rs:140` (`MeshDoc.frames`) | `HashMap<ChangeHash, Verified>` | every applied change, never removed | NONE | 1M x ~4.6 KB ≈ 4.6 GB | grows | snapshot, then drop frames covered by a checkpoint |
| 2 | `habilis-network-doc/src/lib.rs:133` (`MeshDoc.doc`) | Automerge doc | full op history | NONE | not measured | grows | periodic save and compaction |
| 3 | `habilis-network/src/daemon/event_loop.rs:226` (`flush_failed`) | unbounded channel | failed dials, only while the loop stalls | NONE | 1M x ~4.6 KB ≈ 4.6 GB | never drops | reroute to `pending_outbound` or a bounded channel |
| 4 | `habilis-network/src/transport/pool.rs:117` (`PoolInner.addrs`) | `Mutex<HashMap<EndpointId, EndpointAddr>>` | each new peer address from any signed author | NONE | 1M x ~300 B ≈ 300 MB | grows | forget on sweep; FIFO cap at `KNOWN_ENDPOINTS_CAP` |
| 5 | `habilis-network/src/daemon/state.rs:592` (`address_book`) | iroh `MemoryLookup` | each first sighting; only `beacon_arm.rs` removes | NONE (unverified) | ~300 MB per 1M | unknown | forget with row 4 |
| 6 | `habilis-network/src/membership/event.rs:177` (`json_sink`) | unbounded channel | every event line | NONE by design | 1M x ~300 B ≈ 300 MB | grows if consumer stops | bounded; drop app-message lines first |
| 7 | `habilis-network/src/gossip/split.rs:187` (`control_tx`) | unbounded channel | topic events from peer churn | NONE | ~200 MB | grows if stalled | coalesce per peer |
| 8 | `habilis-network/src/transport/graft_backoff.rs:48` (`by_peer`) | `HashMap<EndpointId, Entry>` | each refused neighbor request | NONE in time | 1M x ~110 B ≈ 110 MB | grows | prune after the wait; cap 256 |
| 9 | `habilis-network/src/daemon/state.rs:239, 263` (`direct`, `path_kinds`) | `HashMap` | each probed endpoint; rebind keeps the old endpoint | NONE on rebind path | 1M x 2 x ~50 B ≈ 100 MB | grows | remove old endpoint entries on bind |
| 10 | `habilis-network/src/transport/admission.rs:300` (`closed_on_rung`) | `HashMap<EndpointId, Instant>` | each closed peer | NONE; read checks age, nothing removes | 1M x ~80 B ≈ 80 MB | grows | retain by 1-minute window on insert |
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
| 23 | `habilis-network/src/blob/store.rs:72, 37` (`map`, `owners`) | `HashMap<BlobDigest, BlobEntry>` | each produced blob | `MAX_BLOB_STORE_BYTES` 4 GiB; `evict_to_budget` | 4 GiB on disk | drops oldest, may drop a referenced blob | keep; review referenced blobs |
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
| `event_loop.rs:226` `flush_failed_tx` | unbounded channel | failed flush, one per `watch_flush` task | producers bounded by `PENDING_OUTBOUND_CAP` 1024 | 1024 x 7.7 KB ≈ 7.9 MB | never full | bounded 1024; drop and count when full |
| `transport/ipc.rs:147` `handle_connection` task | task | each local client connection | none on count | unverified | n/a | cap concurrent connections |
| `daemon/state.rs:347` `peers` | set | each distinct nickname heard | time only: `ALIVE_TIMEOUT` 90 s | N x ~66 B | grows within 90 s window | cap at roster size |
| `daemon/state.rs:360` `peer_endpoints` | map | each peer | pruned with `peers` | N x ~250 B est. | n/a | keep |
| `daemon/state.rs:239` `direct` | map | each probed or proven endpoint | pruned only in `forget_peer_endpoint`; not on `NeighborDown` | N x ~50 B | none | remove on `NeighborDown`; unverified for unnamed endpoints |
| `daemon/state.rs:469` `rival_holders` | map | signer keys with rival flag | removed on flag clear and roster retain; no count cap | N x ~72 B; keys free to mint | none | cap with FIFO; unverified whether non-roster signers reach it |
| `daemon/state.rs:74` `ParkedLinkStates` | map | link vectors for authors with no endpoint yet | count cap 256 | up to 16 MB (64 KB per cell, unverified) | refused at cap | cap bytes, not only count |
| `gossip/split.rs:187` `control_tx` | unbounded channel | NeighborUp/Down and default-route items | none | est. 100 B per item | never drops | bound it; drop oldest with a counter |
| `gossip/recv.rs:422` `watch_flush` task | task | one per flushed frame | `PENDING_OUTBOUND_CAP` 1024 | 1024 x 7.7 KB ≈ 7.9 MB | returns frame on failure | bound by dial timeout (unverified) |
| `transport/probe.rs` nudge tasks | task | one per WebRTC rider per alive tick | none on count | small per task | n/a | rate-limit per peer with `Cooldown` |
| `transport/pool.rs:117` `addrs` | map | each address note | removed on `forget`; no count cap | N x ~280 B est. | none | cap at roster size |
| `transport/pool.rs:87` `conns` | map | each dialed or accepted connection | idle sweep; ceiling enforcement unverified | N x conn handle | n/a | explicit cap from `MAX_DIRECT_PEERS` |
| `lookup/mdns.rs:66` `known` | map | each distinct LAN endpoint | removed only on `Gone` | N x ~250 B est. | none | expire without `Gone` |
| iroh `MemoryLookup` (`daemon/state.rs:592,853`) | map inside iroh | every address added | iroh eviction unverified (no vendored source) | unverified | unverified | wrap with a cap if iroh does not evict |
| `membership/join.rs:149` inbound channel | channel | inbound `msg` frames | `INBOUND_CAP` 256 | 256 x 1,408 B ≈ 0.36 MB | drop policy unverified | keep |
| `membership/event.rs:178` `json_sink` | unbounded channel | every node event | none | est. 200 B per event | never drops | bounded; daemon use unverified (tests only in repo) |
| `registry.rs:55` WebRTC `slots` | map | one per attached session | `MAX_DIRECT_PEERS` 64 (`transport/webrtc.rs:678`); enforcement unverified | 64 x per-session size (unverified) | `reserve` refused | verify ceiling check |
| `admission.rs:300` `closed_on_rung` | map | each closed peer | retained by 15 s window | keys in window | n/a | keep |
| `beacon/mod.rs` `RungWatch.tasks` | Vec | one per probe | live probes only | live probes only | n/a | keep |
| `beacon/mod.rs:1761` `arrivals` | channel(32) | arrivals | bounded 32 | 32 x 32 B | drop policy unverified | keep |
| `habilis-network-doc/src/lib.rs:147` `pending` | map | orphan frames | `DOC_PENDING_TOTAL_MAX` 512; `DOC_PENDING_AUTHOR_MAX` 128 | 512 x 3.9 KB ≈ 2 MB per doc | victim evicted | keep |
| `habilis-network-doc/src/lib.rs:162` `dropped` | Vec | orphans dropped | drained by `take_dropped` | per-ingest count | n/a | keep; relies on caller draining |
| `SelfWriteGate` per-peer map (`lib.rs:101-108`) | automerge map | one key per nickname | no cap; no removal on leave in crate | ~100 B per nick est. plus history | n/a | prune on leave (caller unverified) |
| `topology.rs:116` `vectors` | map | each origin's freshest link vector | expiry after 45 s (`topology.rs:23`) | N origins x up to 64 KB (unverified) | refused or expired | add link-count cap |
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

## Open items

- Rows 3, 6, 7, 11, 12, 13 grow only while a consumer stalls. The proof needs a test that stalls the consumer.
- Row 9: no test covers the rebind path.
- Row 18: no count cap. The proof needs a test that feeds 10x the planned cap of origins.
- Row 1 and row 2: the largest real growth path. The proof needs a snapshot design.
- Not read: the ~55 spawn sites in `event_loop`, `gossip/recv`, `transport/probe`, `transport/pool`; `lookup/*`; `membership/join.rs`; webrtc transports; beacon internals beyond the rung and arrival channels; the `SelfWriteGate` map in `habilis-network-doc`; automerge internals; `habilis-network-iroh-multihop-transport/src/graph.rs` and the selector.
- Row 5 depends on iroh internals that were not read.
- Idle counters (uncommitted) were not touched.
