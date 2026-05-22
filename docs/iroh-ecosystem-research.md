# Iroh ecosystem research: top 5 similar repos + learnings

Survey of [awesome-iroh](https://github.com/n0-computer/awesome-iroh),
filtered to the repos most similar to this project (an iroh-gossip mesh
with serverless peer discovery), then studied for reusable learnings.

## Why these 5

Our project is an **iroh-gossip mesh with serverless peer discovery**
(mDNS + mainline pkarr DHT + relay), a **seed-derived topic + rendezvous
bootstrap**, beacon-role migration, heartbeat eviction, and anti-entropy
convergence. Ranked by overlap with *that* (not "uses iroh somewhere"):

| # | Repo | Overlap with us |
|---|------|------|
| 1 | **rustonbsd/distributed-topic-tracker** | Near-twin: DHT-bootstrapped iroh-gossip, no servers, deterministic per-topic keys, partition recovery |
| 2 | **therishidesai/iroh-gossip-discovery** | Gossip-based peer discovery + address book + node expiration (= our roster/heartbeat) |
| 3 | **p2panda/p2panda** (`-net`, `-discovery`) | Production-grade discovery/gossip actors: **healer**, confidential (PSI) discovery, mDNS-vs-internet config |
| 4 | **n0-computer/imsg** | n0's own message-framing building block over iroh — the layer beneath what we hand-roll |
| 5 | **jamessizeland/peer-to-peer** | iroh-gossip chat with presence/roster + neighbor up/down lifecycle (= our `joined`/`left`/`peer_timeout`) |

---

## Repo 1 — distributed-topic-tracker

Closest thing to a published spec of our own design. Bootstraps iroh-gossip
purely from the **mainline DHT** (the `mainline` crate v6 directly, not iroh's
pkarr wrapper) with deterministic per-minute keys.

**Discovery / lookup mechanism:**
- Keys rotate **per unix-minute**: `signing_seed = SHA512(topic_hash + unix_minute)[..32]`,
  `salt = SHA512("salt" + topic + minute)[..32]`. Records are DHT *mutable*
  records keyed by that derived pubkey+salt. Bootstrap always fetches **both**
  `minute` and `minute-1` to cover the boundary.
- Records carry `active_peers[5]` (gossip neighbor node-ids) + `last_message_hashes[5]`.
  A joiner reads a record, then `join_peers()` the publisher + its listed
  neighbors — one DHT record seeds a whole neighbor cluster, not a single node.
- **Public discovery, private content**: DHT key is publicly derivable but
  content is HPKE-encrypted under a shared-secret-derived keypair, with a
  per-record **one-time key** for forward secrecy. Unix-minute coupling gives
  replay protection. Secret rotation via a `SecretRotation` trait.

**Partition recovery (relevant to our heal/anti-entropy):** two background actors
beyond bootstrap:
- **Bubble merge** (small-cluster): if `neighbors < min_neighbors` (4), pull
  peer-ids from DHT records and join up to N new ones.
- **Message-overlap merge** (partition detection): compare local
  `last_message_hashes` against other records'; **disjoint hash sets ⇒ likely
  partition**, so join those publishers to bridge. A cheap partition *detector*
  we lack — our anti-entropy converges sets but doesn't actively detect a split.

**DHT-load hygiene:**
- Per-minute **publish cap**: if ≥5 records exist this minute, don't publish.
- Everything jittered: publisher `10s + 0–50s`; DHT retries `5s + 0–10s`;
  merges `60s + 0–120s`. Per-peer join settle `100ms`, no-peers retry `1500ms`,
  discovery poll `2000ms`.

**Issues:**
- #24 *"Cannot cancel DHT bootstrap"* + 0.2→0.3 rewrite: circular/dangling actor
  refs leaked resources and background tasks **outlived their handle**. Fix =
  **token-gated (CancellationToken) actor lifecycles**.
- #4 *"How do we test 2→10,000 nodes?"* — Docker-compose E2E confirming N nodes
  converge on a topic.

**Learnings:**
1. Steal **message-overlap partition detection** as an active heal trigger.
2. Adopt a **per-minute publish cap** for beacon/rendezvous DHT writes.
3. **One-time-key + shared-secret HPKE** is a ready design for private content + public discovery.
4. Audit our background tasks for the **outlive-the-handle leak**.

---

## Repo 2 — iroh-gossip-discovery

Nodes periodically broadcast a signed `Node` over a shared gossip topic;
receivers maintain an **address book** (`DashMap<name, NodeInfo{last_seen}>`)
with **expiration**.

- Default `expiration_timeout = 30s`; **cleanup runs every `timeout/3`** —
  compare to our `ALIVE_TIMEOUT_SECS` / `SWEEP_INTERVAL_SECS`.
- Records are **Ed25519-signed + CBOR**.
- **Issue #1 "Fix naive gossip peer strategy"**: they `join_peers` *every*
  newly-seen node → fully-connected graph that defeats gossip. Their note:
  *"better off implementing an existing DHT design."* Validates leaning on
  HyParView partial-view + DHT bootstrap rather than all-to-all joining.
  Anywhere we `join_peers` on discovery, cap fanout.

---

## Repo 3 — p2panda (`p2panda-net`, `p2panda-discovery`)

Most mature reference. Built on iroh, uses `ractor`. `gossip/actors/` mirrors
our subsystem split: `healer`, `joiner`, `listener`, `manager`, `receiver`, `sender`.

**`healer.rs` — the in-code comment describes our exact bug:**
> "HyParView can't automatically recover from these fragmentations, this approach makes it possible & gossiping more robust."

Heal is **event-driven, not interval-driven**:
- Subscribe to **address-book changes** → on change, `JoinNodes(...)` to
  re-bridge fragments.
- **Also re-join when our own transport info changes** — i.e. "we went
  offline/degraded and came back," precisely our **post-sleep mesh-collapse**.
  We heal on a fixed 15s timer; p2panda triggers heal *on the
  connectivity-recovery event itself*.

**Discovery config & issues (maps to our `--mdns`/`--dht` allowlist):**
- `DiscoveryConfig`: `random_walkers_count: 2`, `reset_walk_probability: 0.02`
  — discovery is a **random walk** over the overlay.
- **`p2panda-discovery` does confidential discovery via PSI / Private Equality
  Testing** (`psi_hash.rs`): two nodes exchange data only once both prove
  knowledge of the same topic — topic never leaked to outsiders.
- Relevant issues: #1079 "Allow discovery only over mDNS or internet",
  #1092 "Option to disable mDNS altogether", #1140 "mDNS Discovery Test
  Sometimes Fails" (flaky mDNS in CI), #1181 event-stream prematurely
  terminated on a duplicate, dropping downstream events.

**Learnings:**
1. **Event-driven heal**: wire our resume/connectivity-edge signal straight
   into a heal trigger instead of relying solely on the 15s timer.
2. Expect **flaky mDNS tests**; gate/retry them.
3. PSI-based discovery is the upgrade path if "id leaks the topic" matters.

---

## Repo 4 — imsg (n0-computer)

n0's experimental **message-framing layer** over iroh QUIC: whole-message
`Bytes`, either side sends first, easy to reason about which message is lost on
close. Codec via `tokio_util::codec` (`FramedRead/Write`) + varint length prefix;
a `Control` stream (opened first) carries close frames distinct from the `User`
message stream. **Deliberately ALPN-less.**

**Learning:** the building block under our hand-rolled gossip framing. Their
varint-prefixed framed codec is the idiomatic pattern for any future
direct-stream path; their explicit close semantics (know which messages were
lost at shutdown) is a cleaner model relevant to our graceful `left` announce.
Experimental — adopt ideas, not the dep.

---

## Repo 5 — jamessizeland/peer-to-peer

iroh-gossip Tauri chat (forked from n0 browser-chat). Closest to our
presence/lifecycle surface. `chat/peers.rs` keeps `PeerMap<NodeId,
PeerInfo{status,last_seen,nickname}>` driven by events:
- `Joined{neighbors}` seeds peers as `new_starters`; a peer surfaces only when
  its first **`Presence`** (nickname) arrives — **exactly our join-horizon /
  "surfaced ⊆ participants" distinction**, independently arrived at.
- Three-state liveness: `Online → Away (>10s silent) → Offline (NeighborDown)`.
  Their intermediate **"Away"** soft state before hard eviction is a nicer UX
  than our binary timeout.
- They diff `before != after` and only emit on change — same dedup discipline
  we want (and the bug p2panda #1181 warns about).
- `ticket.rs` (room = topic + bootstrap addrs) **embeds peer addresses** — the
  thing we deliberately avoid (creator-independence). Confirms our design choice.

**Learning:** consider a soft **"Away"** tier between alive and
`peer_timeout`-evicted.

---

## Cross-cutting takeaways (highest value first)

1. **Event-driven heal on connectivity-recovery** (p2panda) — wire our
   sleep/resume edge detector into an immediate heal kick. Most direct fix for
   post-sleep mesh collapse without touching the destabilizing `HEAL_INTERVAL`.
2. ✅ **Partition re-bridge** (dtt). *Done — but reframed on investigation.* Our
   `tick_heal` already re-grafts the rendezvous unconditionally every 15s, so we
   never had dtt's "no recovery" gap (dtt needs DHT overlap-detection precisely
   because it has no always-on rendezvous). The real residual gap was
   *rendezvous-dependence*: we dropped peer ids on `NeighborDown` and only ever
   re-grafted the rendezvous, so a flapping relay could strand halves that still
   held each other's direct addresses. Fix: a bounded `known_endpoints` cache
   (survives `NeighborDown`) + `heal::rebridge_known`, re-dialing remembered
   peers directly on the isolation signal (hard/resume edge or zero live links).
   No DHT, works in both modes. Verified via the resume reliability test.
3. **Cap join fanout on discovery** (iroh-gossip-discovery #1) — never
   `join_peers` every node seen; it collapses gossip into all-to-all.
4. **Per-minute DHT publish cap** (dtt) — throttle rendezvous/beacon DHT writes
   by record-count, not just time.
5. **Token-gated task lifecycles** (dtt 0.2→0.3) — audit our Monitor/daemon tree
   for tasks that outlive their handle.
6. **Crate ideas**: `mainline` directly for DHT control; `tokio_util::codec`
   varint framing (imsg); `ractor` for actor structure (p2panda).
7. **Test reality**: expect flaky mDNS CI (p2panda #1140); Docker-compose N-node
   convergence (dtt) is the scaling-test pattern.
8. **Privacy upgrade paths**: HPKE one-time-key content encryption (dtt) and PSI
   topic discovery (p2panda).

---

# Issue deep-dive: common problems across the 5 repos & how they were solved

The 4 small repos have few issues (dtt: 3, gossip-discovery: 1, imsg: 1,
jamessizeland: 4 feature requests). p2panda has 400+ — the real corpus. Below are
the recurring problem **clusters**, several of which match bugs we have already
hit or are exposed to. Issue numbers are p2panda unless prefixed.

## Cluster 1 — Fixed-node-id reconnect stalls *(highest priority for us)*

- **iroh-gossip#10** (upstream, long-lived) + p2panda **#695** "Peers do not
  re-connect when using fixed node ids" (priority).
- **Root cause** (matheus23, iroh-gossip#10): iroh-gossip *rejects* a new
  incoming connection if it already holds an `accept()`ed one. So after a quick
  disconnect, the **stale connection must time out (minutes)** before a reconnect
  with the *same* node id succeeds. Maintainer's prescribed fix: a connection
  manager that *always accepts* incoming, dedups dialed-vs-accepted by comparing
  node_ids ("you manage connections you initiate; the peer manages theirs").
- **Community workaround**: randomize your gossip node id on every
  (re)connect, and run a *second* permanent endpoint as a resolver that maps
  your stable id → current random gossip id.
- **For us**: we use a **deterministic seed-derived rendezvous/beacon identity**.
  This is very likely a contributor to our **post-sleep mesh-collapse**, and it
  *validates our recent "hard re-bootstrap on resume" commit* — re-bootstrapping
  sidesteps the wait-for-stale-timeout trap. Worth confirming our beacon-migration
  path doesn't get wedged behind a stale accepted connection on the rendezvous id.

## Cluster 2 — Background tasks outliving their handles / lifecycle leaks

- dtt **#24** "Cannot cancel DHT bootstrap": dropping `Topic`/`Sender`/`Receiver`
  left `publish`/`bootstrap`/`bubble_merge`/`overlap_merge` tasks running. Fixed
  in 0.3 with **token-gated (CancellationToken) lifecycles**.
- p2panda **#967** "Gossip session gets prematurely dropped", **#639**
  "auto-unsubscribe when both tx & rx dropped", **#890** "move actor init into
  `post_start()`", and the big **#818 [TRACKING] Net rewrite with supervised
  actors**: a full rewrite onto a `ractor` **supervision tree** — "stop all
  dependent child actors when connection closes/fails", "allow endpoint to be
  recreated on failure".
- **For us**: audit the Monitor/daemon task tree for tasks that outlive their
  handle; ensure drop/close tears down children. Supervised-actor structure is
  the ecosystem's converged answer.

## Cluster 3 — Oversize gossip messages silently dropped *(critical, easy to hit)*

- **#628**: publishing a gossip message **> ~4057 bytes** — `.broadcast()`
  returns `Ok`, but iroh-gossip never sends/delivers it, **no error**. **#629**
  "make max gossip message size configurable", **#716** "subtract-with-overflow
  panic in gossip buffer".
- **For us**: we accept **arbitrary-length UTF-8 message bodies**. A long body
  will silently vanish at the iroh-gossip layer. We should **cap/validate (or
  chunk) message size and surface an explicit error** rather than silently drop.

## Cluster 4 — Mesh formation fragility (the "third peer" problem)

- **#723** "Third peer (sometimes) fails to join topic gossip", **#685**
  "Establishing connection with bootstrap peer fails when direct address unknown"
  (gossip used the address-book addr instead of the freshly *resolved* one),
  **#683** "don't duplicate known peer addresses on discovery", **#659** "inject
  peer direct addresses dynamically".
- Resolution direction: feed *resolved* addresses into gossip; the **healer**
  re-joins from the address book (Cluster matches our heal/anti-entropy need).
- **For us**: reinforces event-driven heal + making sure resolved rendezvous
  addresses (not stale cached ones) are used on join.

## Cluster 5 — Discovery-mode configuration (mDNS vs internet)

- A whole saga: **#939** "remove `MdnsDiscoveryMode::Disabled`" → **#1092**
  "option to disable mDNS altogether" → **#1079** (open) "allow discovery only
  over mDNS *or* internet". Plus **#903** "compilation breaks on disabled `mdns`
  feature flag" and **#956** "rename `MdnsDiscovery` → Resolver/AddressLookup".
- Lesson: getting the discovery-toggle API shape right is genuinely hard; they
  churned between feature-flag, enum-mode, and modular-import approaches.
- **For us**: validates our `--mdns`/`--dht` **allowlist** design (and the
  hard-error-if-no-`--public` rule). #956's rename toward "AddressLookup"
  matches our own "discovery (lookup)" terminology — good naming confirmation.

## Cluster 6 — Event-stream noise, dedup, and asymmetric lifecycle

- **#1181** "`ManagerEventStream` prematurely terminates when duplicate detected
  → dropped events", **#718** "spammy `gossip joined` / `peer discovered`
  events", **#1179** "some sync sessions emit *started* but not *ended*",
  **#964** "confusing semantics between `SyncFinished`/`LiveModeFinished`/
  `Success`".
- **For us**: directly validates our **symmetric lifecycle invariant** (a
  departure surfaced only if the arrival was) and join-horizon dedup — #1179
  (started-without-ended) is exactly the asymmetry we guard against. Watch that
  our event stream doesn't *terminate* on a dedup hit (#1181).

## Cluster 7 — `tokio::select!` all-branches-disabled panic

- **#898**: a `select!` where every branch's precondition is false and there's
  no `else` branch **panics** at runtime ("all branches are disabled and there
  is no else branch").
- **For us**: audit our `select!` loops — add an `else`/default arm where
  branches can all become disabled.

## Cluster 8 — Flaky mDNS / discovery / sync tests

- **#1069** "flaky `discovery::tests::smoke_test`", **#1140** "mDNS Discovery
  Test Sometimes Fails", **#941** "flaky `topic_log_sync_failure_and_retry`".
- **For us**: expect our subprocess mDNS tests to be flaky; budget retries/gating.

## Cluster 9 — iroh upgrade churn

- **#1090** (→0.97), **#960** (→0.96), **#915** "iroh v0.96 fails when port is
  already used" (a real regression surfaced by upgrade).
- **For us**: treat each iroh bump as a behavior-change risk; keep the
  wire-contract + reliability subprocess tests as the upgrade gate.

## Cluster 10 — Stale-peer / heartbeat reporting

- **#652** "Report stale peers after some duration" (track activity table; emit
  a stale/disconnected event), **#597** "add peer to address book from gossip
  `NeighborUp`". jamessizeland adds a soft **Away** tier (>10s silent) before
  Offline.
- **For us**: matches our `quiet`/`ALIVE_TIMEOUT` model; the soft **Away** tier
  is a possible UX improvement over binary eviction.

## Cluster 11 — Sync/anti-entropy session restart

- **#901** "failed sync sessions are not restarted", **#701** "sync errors
  should lead to re-attempts", **#630** "reset sync state after disconnect".
- **For us**: ensure a failed anti-entropy reconcile retries and resets cleanly
  after a disconnect rather than wedging.

## Most actionable for us (ranked)

1. **iroh-gossip#10 fixed-node-id reconnect** — confirm beacon migration / resume
   path isn't wedged behind a stale accepted connection; validates hard
   re-bootstrap on resume.
2. **#628 oversize-message silent drop (~4057 B)** — cap/validate/chunk UTF-8
   bodies and return an explicit error.
3. **#898 `select!` panic** — audit loops for a missing `else` arm.
4. **#818 supervised-actor lifecycle** — audit our task tree for handle-outliving
   tasks; teardown children on close.

---

# Top 5 by GitHub stars: issue sweep & cross-repo learnings

A second pass ranked **by popularity, not similarity**. These are big iroh
*consumers* (file-share, a game, a collaborative editor, an AI-training network),
so the transferable signal is in their **iroh transport / connectivity /
relay / discovery** issues — exactly the layer we also depend on.

| Rank | Repo | ⭐ | Domain | Open/Closed issues |
|---|---|---|---|---|
| 1 | tonyantony300/alt-sendme | 8027 | File sharing (Tauri, wraps sendme) | 27 / 47 |
| 2 | fishfolk/jumpy | 1840 | 2D game, rollback netcode | 78 / 214 |
| 3 | teamtype/teamtype (ex-ethersync) | 1826 | Collaborative text editing | ~50 / ~90 |
| 4 | n0-computer/sendme | 1022 | iroh's flagship file-send demo | 18 / 16 |
| 5 | PsycheFoundation/psyche | 800 | Decentralized AI training (gossip + blobs) | 74 / 88 |

Star counts and issue lists current as of 2026-05-22.

## The single strongest signal: fixed-node-id reconnect *(now 3 repos + upstream)*

- **psyche #25** "client with fixed ID can't reconnect in same epoch" — the
  *same bug* as p2panda **#695** and upstream **iroh-gossip#10**. Three
  independent iroh consumers hit it.
- Confirms it's not project-specific: **iroh-gossip won't quickly re-admit a
  reconnecting peer that keeps a stable node id** — the stale accepted
  connection must time out first.
- **For us**: we use a **deterministic seed-derived rendezvous/beacon identity**
  — squarely in the blast radius. This is now the best-evidenced explanation for
  our post-sleep mesh-collapse, and the strongest argument that "hard
  re-bootstrap on resume" is the right mitigation. Worth an explicit test:
  SIGSTOP/SIGCONT a member and assert it re-admits without waiting out a
  multi-minute timeout.

## Relay is the other big theme — and it's mostly bad news

- **psyche #313** "add no-relay mode… relaying *never* works and always breaks
  things; force-disconnect if a node becomes relayed, wait a few seconds for a
  direct connection, else drop." A heavy-bandwidth user's blunt verdict.
- **sendme #121 / alt-sendme #121** "Connection Timeout due to Relay
  Infrastructure Mismatch (Production vs Canary)" — maintainer (matheus23):
  *different iroh versions use different relays and aren't guaranteed
  compatible*; relay version skew silently breaks connections.
- **alt-sendme #58** "Add TURN relay (CGNAT support)" + **#62** "Frequent Upload
  Disconnections" + **#63** "proxy relay": **CGNAT / symmetric NAT defeats
  hole-punching and the relay fallback sometimes doesn't activate** — transfers
  work on LAN/cone-NAT but fail mobile-to-mobile.
- **sendme #32 / #112 / #67**, **psyche #586 "add iroh network diagnostics"**,
  **#214 "iroh testing"**: users *repeatedly can't tell whether a connection is
  direct or relayed*, and ask for it in logs.
- **For us**: (a) we **hard-pin a default relay** — sendme #121 says that's
  fragile across iroh upgrades; pin the *relay* and *iroh* versions together and
  treat a bump as a connectivity-contract change. (b) Surface **connection type
  (direct vs relay) + relay URL** in our diagnostics/logs — it's the
  most-requested observability gap in the whole ecosystem. (c) Expect **CGNAT
  failures**; document that public mode needs the relay and may still fail on
  symmetric-NAT-both-ends.

## Robustness clusters (recurring across the consumers)

- **Transport errors must not crash the process**: teamtype **#289** "Error in
  iroh connection can crash the daemon", **#145** "Daemon sometimes crashes when
  peers disconnect" (prio high), **#150/#194** panics on edge inputs. *For us*:
  audit that an iroh/gossip error path can never panic the daemon — degrade and
  re-bootstrap instead.
- **Auto-reconnect is hard to get reliable**: teamtype **#380** "p2p
  reconnection doesn't work all the time", **#196** "when disconnecting, try to
  reconnect", **#102** "reconnect after daemon restart"; jumpy **#769** "Improve
  Network Error Recovery", **#970** "Handle Network Disconnects". *For us*: our
  heal + re-bootstrap is the equivalent; keep it the single, well-tested path.
- **Gossip churn on bad connections**: psyche **#78** "reduce gossip swarm churn
  on bad connections". *For us*: a flapping peer shouldn't thrash the mesh —
  consider backoff before re-adding a peer that keeps dropping.
- **Membership/allowlist enforcement**: psyche **#38** "force-disconnect all
  clients no longer in the allowlist", **#30** "only download from peers in the
  previous epoch". *For us*: parallels our roster; if we ever add access
  control, eviction must actually drop the transport connection, not just hide
  the participant.
- **Resume/rejoin after pause**: psyche **#357** "fails to get model via P2P
  when rejoining a paused run". *For us*: same family as our sleep/resume edge —
  keep the resume-rejoin path explicitly tested.
- **Bounded storage**: psyche **#323** "verify the iroh blob store doesn't grow
  indefinitely". *For us*: our 200-message ring buffer already caps this —
  validated design choice.
- **Edge-case inputs panic**: sendme **#87** "empty directory → BLAKE3 hazmat
  assertion failure". *For us*: we accept arbitrary UTF-8 bodies — keep guarding
  empty/degenerate inputs (we already reject control chars).
- **Human-readable identity over node IDs**: teamtype **#321** "show human
  readable name instead of node IDs", **#408** "identify more clearly who
  connected". *For us*: our nickname layer already does exactly this — validated.

## Concrete suggestions for our project (ranked by value)

> Status legend: ✅ done · ⬜ open. The top 3 were implemented 2026-05-22.

1. ✅ **Fixed-node-id reconnect reliability test** (psyche #25 / p2panda #695 /
   iroh-gossip#10). Added `test_fixed_id_reconnect_admits_fast` in
   `tests/gossip_network.rs`: SIGSTOP/SIGCONT a member (same process ⇒ same
   endpoint id), probe immediately on resume, require delivery within 50s —
   far below iroh's multi-minute stale-connection timeout. Confirms our resume
   re-bootstrap keeps re-admission heal-bound (~10s observed).
2. ✅ **Connection type + relay URL in diagnostics** (sendme #67/#112/#32,
   psyche #586). Added `gossip::conn_path()` classifying each link
   `direct`/`relay`/`mixed`/`unknown` + relay URL. Logged on `NeighborUp`
   (point-in-time) and per-peer in the periodic neighbor census (representative,
   post hole-punch). File-sink only — no wire/`--output json` change.
3. ✅ **Relay + iroh version coupling** (sendme #121). Confirmed our pinned
   `RENDEZVOUS_RELAY` *is* iroh 0.98.2's `defaults::prod` NA-east relay (the
   `iroh-canary` host is iroh's prod domain at this version, not staging — the
   existing comment was already correct). Added tripwire test
   `pinned_relay_matches_iroh_prod_na_east` that fails if an iroh bump moves the
   prod relay off our host, forcing manual review before shipping.
4. ✅ **No-panic audit** (teamtype #289/#145). *Audited — already covered, no
   change.* The event loop logs a gossip `Err` and continues, and a terminal
   `None` flips `gossip_open=false` while IPC keeps working; `connect`/`bind`/
   `resolve` (`probe_connect`, `add_peer_addr`, `beacon`) return `Result` and
   degrade; parsing uses `let Ok(…) else { return }`. The only non-test
   `expect`/`unwrap` are documented infallible invariants on constant/internal
   data. A transient iroh error can't panic the daemon. (One startup `expect` on
   SIGTERM-handler registration is genuinely fatal-at-startup and left as is.)
5. ✅ **CGNAT / relay-limits note** (alt-sendme #58/#62). Documented in
   `docs/gossip.md` (Transport section): symmetric-NAT/CGNAT on both ends can
   fail even in public mode, and the relay is the fallback, not a toggle —
   verified in `build_endpoint_for_mode` that public mode never disables the
   relay (default participant rides iroh's multi-relay set; `--relay` only
   changes it). No code change.
6. ⬜ **Backoff for flapping peers** (psyche #78). *Deferred — speculative.* No
   observed mesh churn, and the reclaim window + 15s-bounded re-dial already
   rate-limit reconnects. Build only if churn shows up in logs.
7. ✅ **`select!` else-arm** (p2panda #898). *N/A — confirmed.* Our loop's
   interval-tick arms are always live, so "all branches disabled, no else"
   can't occur. (Message body size is also already capped at 16 KB with a clear
   error — `test_message_size_limit` — so p2panda #628 is handled too.)
