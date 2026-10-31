# Idle CPU: where it goes and how to cut it

Problem: an idle `agent-gossip` daemon uses near 10% of one core on macOS.
A machine often runs 10 or more daemons, so the cost adds up.

Each idea has three parts: the idea, how to run the experiment, and the
result. A result of `pending` means nobody ran the experiment yet.
Nothing here is measured unless the Result says so.
`[V]` marks a claim verified from a log, a source line or a command. `[G]`
marks a guess.

Requirements:

- No regressions. The test suite stays green. Each change has a test or a
  measurement that proves it.
- No change of behaviour, unless it gives a measured performance gain. A change
  that alters behaviour states what changes and shows the gain that pays for it.
- Everything rests on evidence: measured numbers, paired A/B, the noise level.
  An idea without a measurement stays an idea.

Commits: agent-gossip `main` is `b588fc75`. `habilis-network` `main` is
`f7cd9bbc` (the rev the app pins). Every report carries its sha. Most live
daemons still run the installed release 0.12.10 (`f5a4459`), older than `main`.

Sources: wakeup inventory (`tale-gorge`), advisor assessments (`ignite-bench`,
two rounds), census series from the live gossip (`valve-comet`, `lumen-gear`,
`wire-straw`), and measurement runs (`layer-oval`, `wire-straw`, `lumen-gear`,
pending).

## What we know now

### The cost is received frames, not own timers

- [V] The census line in every daemon log (`mesh census ...`, every 10 s) has
  the counters. In a calm mesh (15:14, advisor) `idle_external` was 78 to 160
  per 10 s (8 to 16 wakeups per second). In the live gossip later it was:
  - `valve-comet`: 4,000 to 11,000 per 10 s, CPU average 16% of one core over
    80 min.
  - `lumen-gear`, `wire-straw`, `fig-grand` on `nyc-macmini1`: about 16,600 per
    10 s (1,660 per second), CPU 67% to 91% of one core. `wire-straw` measured
    21.3 s of CPU in 30 s.
- [V] The daemon's own timers stay small in all of these (reclaim 18 to 24 per
  10 s, the others 0 to 2).
- [V] Neighbour up and down events are about 0.4 per second, so they are not
  the wakes (N2 of the advisor). The wakes are received frames.
- [V] The hottest on-CPU leaves are `__sendmsg` and `__recvmsg` (symbols are
  stripped, so the caller is unknown).

### CPU follows the wire traffic, which is about 1,000 times the logged messages

All [V], leader's live daemon `valve-comet` (pid 64713, release 0.12.10), taken
at about 13:00 local, 83 min of uptime:

- Logged inbound frames: 14,000 in 83 min, about 2.8 per second, about 6 KB per
  second of bodies (257 distinct state frames received 7,883 times, 1,160
  distinct presence frames received 6,140 times).
- Wire traffic (`nettop -P -p <pid>`, one 5 s window): in 1,970 packets/s and
  1.78 MB/s; out 3,390 packets/s and 4.0 MB/s. Cumulative: 2.66 GB in and
  3.04 GB out. So the wire carries about 1,000 times the bytes of the logged
  frames, and the daemon sends 2.2 times what it receives.
- Kernel and context switches: 13.1 million BSD syscalls and 13.1 million
  context switches in 83 min (about 2,600 per second of each). 42% of the CPU
  time is kernel time (7:00 of 16:38). Only one of 4 threads burns CPU, so the
  daemon is capped at one core.
- Per packet the cost is ordinary: 0.25 core divided by 5,350 packets/s is about
  47 microseconds per packet (decrypt or encrypt, parse, syscall). So no single
  handler is slow. The packet count is the problem.
- [G, arithmetic] The resend flood predicts the volume: N members x 3 answerers
  x 64 resends per digest every 10 s is 43 x 3 x 64 / 10 = about 825 frames per
  second reaching each member. At 1.9 KB per state frame that is about 1.5 MB/s
  in, against 1.78 MB/s measured. Frames the member already holds are dropped
  silently, which is why the log shows only 2.8 per second. This needs I6
  (duplicate-frame counter) to confirm.
- Note: a first lsof listing of TCP sockets mixed in other processes (`lsof -p`
  with `-i` is an OR). It is not used here.

### Live test of the resend flood (H7), [V]

Run on the live gossip with an observer daemon (release 0.12.10, a fresh member,
`RUST_LOG=habilis_network::gossip=debug` so the answer path logs
`anti-entropy: resent messages a peer was missing`). 90 s settle, then a 240 s
window. Host load average 54. Script: `flood-test.ts` in the session scratchpad.
Prediction before the run: each member answers about 3 digests per 10 s, up to 64
frames each, so about 190 resends per 10 s (19 per second).

- 66 digest answers in 240 s (0.275 per second, the prediction was 0.3).
- 3,151 frames resent in 240 s by this one member: 13.1 frames per second, or 131
  per 10 s (69% of the predicted 19 per second). The mean answer resent 47.7
  frames and the largest hit the cap of 64. So the missing set is almost always
  larger than the cap: the loop is perpetual.
- 36 more digests were ignored by the one-serve-per-author-per-window gate.
- With about 45 members all doing the same, the mesh floods about 45 x 13.1 = 590
  frames per second, and each member receives them. The observer measured 0.9 MB
  per second in (818 packets per second) and 1.77 MB per second out (1,630
  packets per second), at 24.6% of one core. At 1.9 KB per frame, 590 frames per
  second is about 1.1 MB per second, close to the 0.9 MB per second measured.
- Result: the volume of the resend flood matches the traffic and the CPU. The
  prediction held within 31%. H7 rises from 80% to about 90%. Still unknown:
  which requester asks and why it never stores the frames (`origin-magnet`).
- Limits: the observer was new (a smaller log than an old member, so this is a
  low estimate); the host was loaded; the debug filter replaced the default
  filter, so the census and the frame counters were not logged in this run.

### The same frames come in again and again

All [V] from the advisor's own log, 15:30 to 16:26:

- 8,343 inbound frames logged (5,765 state, 2,574 presence).
- One presence frame (author `origin-magnet`) was received 571 times, 10 to 20
  per minute for more than an hour.
- Six state frames about 3.5 days old were received 67 to 69 times each, about
  2 per minute each, without a break.
- Of the inbound frames in 16:xx, 1,721 are older than 1 day, 1,409 are
  between 1 h and 1 day, and 1,019 are younger than 1 h. Most inbound traffic
  is old frames.
- Per 10 min, the roster stayed below the cap while churn grew: up/down events
  6/3, 101/101, 147/150, 137/141, 249/245. `idle_external` grew with them
  (534, 2,249, 3,184, 3,769, 7,238 per 10 s) and so did `idle_resent`.
- 15 "maintenance timer stalled ... suspected=throttle" lines show that the
  host was starved at that time. The host load average was 30 to 185.

### Why a frame can come back forever (code, [V] unless marked)

Engine `f7cd9bb`, `iroh-gossip` `4e2c14e`:

- C1. A digest answer re-broadcasts each missing frame to the whole mesh with a
  new nonce, so the gossip dedup cannot drop it
  (`gossip/antientropy.rs:311-319`). The budget is 64 resends per digest, 3
  answerers, one serve per author per 5 s, and each member sends a digest every
  10 s. The comment at `antientropy.rs:254-257` already names the flood risk.
- C2. Nothing remembers that a frame was already resent to a requester. One
  member that keeps listing a frame as missing makes up to 3 answerers flood it
  to all N members every 10 s. Each copy is a `Swarm` broadcast with IHAVE
  fan-out, which multiplies it by about N control frames.
- C2b. [V by reading, `origin-magnet`, not yet run] The answer is a pure function
  of the answerer's log, the requester's have-set and the window
  (`daemon/message_log.rs:353`). The only gate is one serve per digest author
  per window (`event_loop_state.rs:1712`). So a frame the answerer holds and
  the requester never holds is resent to the whole mesh every window, for
  ever. Three ways in code that a requester never holds a frame: (a) a stale
  Joined or Alive presence frame is not retained by the receiver
  (`recv.rs`, `retained_after_lifecycle`) while a member that still holds it
  keeps resending it; (b) a full log evicts the oldest frame of the fullest
  author at once, so a very old frame can be evicted on arrival; (c) the
  loggable class differs between versions. [G] The 571-times presence frame is
  (a). The 3.5-day state frames use the separate state digest path, and the
  reason is not found yet.
- C3. `iroh-gossip` refill loop (`proto/hyparview.rs:875-928`): a node below
  the active view capacity dials one passive peer, and a full peer refuses a
  low-priority neighbour request. The 500 ms timer then retries, with no
  backoff and no bound. With a roster above the cap (43 against 32), full
  passive peers are the normal state. [G] This is a 2 Hz dial loop per node.
- C4. Join paths evict a random link on a full peer (`hyparview.rs:1015-1019`),
  and the engine uses Join for the rendezvous graft every 15 s and for
  re-bridging. Each eviction starts the C3 loop on another node. The engine's
  own fills use backoff (1 to 15 min), so the engine side is damped and C3 is
  not.

### Scaling

- [V] Release 0.12.10, private gossip on the leader's host, CPU per daemon:
  1 daemon 0.125%, 5 daemons 0.19%, 10 daemons 0.56% (`layer-oval`). The
  advisor saw 4.9% at about 24 peers in the live gossip. So a calm small gossip
  is cheap, and the cost grows with churn and size, not with timers.

### Prior work

Commit `eb7900b0` removed the `RTM_MISS` rebuild of the interface state (2.01%
to 0.06% of one core). Commit `8b63e105` moved to a single-thread runtime.

## Hypotheses

| | Hypothesis | Verdict | Confidence |
|---|---|---|---|
| H7 | Anti-entropy resend loop: a frame that some member never stores is re-flooded mesh-wide forever (C1, C2) | supported by the repeat counts | 80% [G on the requester] |
| H6 | Churn amplification: links drop under load, return, and trigger resends and refills | supported as a trigger | 70% |
| Cap | Roster above `--max-peers` makes C3 run without end | plausible, but churn started below the cap too | 50% |
| H1 | `Swarm` IHAVE fan-out (about N control frames per broadcast) | a multiplier of H7, not a root cause | 40% as a main cost |
| H2 | Per-connection QUIC upkeep (5 s keep-alive) | demoted | 25% |
| H3 | The daemon's own maintenance cadence | killed as the main driver | 90% |
| H4, H5 | Event-loop baseline and per-endpoint network upkeep | killed as main costs | 90% |

## Experiments that separate the causes

Run on one quiet host: a load average below the core count for the whole run,
`uptime` logged every minute, and zero "maintenance timer stalled" lines
(otherwise the run is void). Private loopback gossip, release build of the
same rev, 10 min idle after the last join. Use only what exists today:

- count A: census `idle_external`, `idle_resent`, `link_len` per 10 s;
- count B: "gossip neighbor up/down" lines per 10 min;
- count C: repeated receptions. Group inbound log lines by type, author and
  timestamp (`sort | uniq -c`). A frame received more than 5 times is a loop.
  No message bodies are needed;
- count D: "maintenance timer stalled" lines (must be 0);
- count E: CPU from `ps -o time` over the last 5 min.

Runs, each pair twice and alternated:

- R1, idle floor: N=12, `--max-peers 32`. Expect about 0.3 x N wakeups per
  second, no repeats, no churn.
- R2, cap exceeded: N=12, `--max-peers 8`. Same mechanism as 43 against 32, at
  a size one host can carry. Compare with R1.
- R3, host overloaded: N=12, `--max-peers 32`, and `SIGSTOP` 3 random daemons
  for 20 s every 60 s. This gives stalls without CPU noise from a load
  generator.
- R4, resend loop alone: R1, then start 1 member with a fresh state file 5 min
  later. Count C for the next 10 min. A frame received more than 5 times with
  no churn confirms H7 without load or cap.

Do not run 16 to 24 daemons on one host: the test itself then overloads the
host, which is the confound.

Result of R1 to R4: pending. The earlier E1 to E5 plan is replaced by this.

## Metadata traffic: facts and ideas

The user's observation: most of the data on the network is not data that the
users of the network created. It holds, all [V] unless marked.

### Facts

- Inbound frames logged by `valve-comet` in 83 min, by bytes: 8,272 state
  frames (all on channel `meta`) carry 15.66 MB (98%); 493 directed frames
  (tasks and replies) carry 0.30 MB (2%); 7 chat messages carry about 3 KB. Peer
  presence frames (6,638) are not counted by bytes, because the log does not
  size them. The wire carried 2.66 GB in, so the logged frames are about 0.6% of
  it, and the resends hide the rest.
- 45% of the state frames received (3,551 of 7,896) are authored by peers that
  are no longer in the roster.
- The shared meta document is 103,845 bytes for 45 peers. 14 of its 45 entries
  belong to peers that left (30,835 bytes, 30%), and they are never removed.
- A peer entry is 2,410 bytes. 2,086 of them (87%) are the agent card, which a
  peer never changes while it runs. 83% of the card bytes are identical across
  at least 80% of the peers: extension descriptions, skills, interface and input
  and output modes (1,988 static bytes against 418 unique bytes). The unique
  bytes are the endpoint id, the key, the nickname and the version.
- The whole document compresses from 103,845 to 9,078 bytes with gzip (91%
  smaller). One peer entry alone compresses from 2,410 to 1,254 bytes (48%).

### Ideas

Ranked by how much traffic they remove. Cutting the number of transfers (1 to
3) removes far more than shrinking each frame (4 to 6): new metadata is created
slowly (257 distinct state frames in 83 min), and the traffic comes from sending
the same frames again.

1. Stop the resends (M1 below). A frame is sent once to a member that needs it,
   not to the whole mesh every 10 s. Expected: most of the wire traffic, if the
   arithmetic above holds.
2. Remove the state of members that left. When a member is gone for a time T,
   a merge patch with `null` deletes its entry (RFC 7386), and its old patches
   leave the logs, the digests and the answers. Removes 30% of the document and
   45% of the state frames received. It changes behaviour: the history of
   peers that left disappears from `status`. Experiment: count frames by author
   in or out of the roster before and after, on a 12 member run with churn.
3. Use state heads, not the frame log, to repair the `meta` channel. The state
   channel already has heads digests and direct state sync (the fast round).
   Anti-entropy of the frame log then needs to offer no `meta` frame at all.
   [G] this may be what the 3.5 day old frames come through; `origin-magnet`
   checks it. Experiment: R4 with `meta` frames excluded from the answer, then
   check that a late joiner still converges.
4. Send the card by reference. A card is static, so the wire carries its unique
   fields (endpoint id, key, nickname, version: 418 bytes) plus a template
   version, and the receiver rebuilds the static part from the version it
   knows. Falls back to the full card for an unknown version. Saves about 1.7
   KB of 2.4 KB per peer entry (about 70%). It changes the wire format of the
   app-level card, so it needs a version bump and a mixed-version plan.
   Experiment: size of a join backfill before and after, at 12 members.
5. Compress frame bodies. gzip gives 48% on one entry and 91% on the whole
   document. A frame-level compression also helps every other JSON body.
   It changes the wire format, so it needs a version bump. Experiment: add
   compression behind a const in a worktree and compare bytes and CPU per
   frame at 12 members, because compression costs CPU on both sides.
6. Make the join backfill one transfer. A new member now gets the roster, the
   presence frames (1,160 distinct) and the meta patches through gossip
   floods. One unicast snapshot from one member (the fast round already exists)
   plus a horizon for the joiner would replace it. Experiment: bytes received
   in the first 5 min after a join, before and after.
7. Send maintenance digests to the 3 chosen answerers only. Today every member
   hears every digest and only 3 answer. [G] small gain; it matters only after
   1 to 3.

Not an idea: dropping metadata that users need. Peer cards, nicknames and
status stay; only the repeats and the stale copies go.

## Proposal: remove anti-entropy

The user proposes to remove the whole anti-entropy mechanism: it costs CPU and
adds complexity, and if an agent misses a message the sender should send it
again. Status: under review by the advisor (task `ignite-bench`), and no
experiment has run.

Facts for the decision (engine `f7cd9bb`, [V] by counting):

- Complexity: `gossip/antientropy.rs` has 2,115 lines, plus 307 lines in 5
  sub-files (`fast_rounds`, `serve_budget`, and others), 2,422 lines in all.
  29 files mention it, including `transport/send.rs` (27 mentions),
  `daemon/event_loop.rs` (27), `gossip/recv.rs` (23) and
  `daemon/state/event_loop_state.rs` (23). It has hidden flags
  (`--antientropy-interval-secs`, `--antientropy-max-resend`) and its own tuning
  constants (interval 10 s, window 70 ids, 64 resends, 3 answerers, a serve
  budget).
- What it is for, as the tests name it: `late_joiner_backfill_loopback`,
  `resend_after_loss_loopback`, `missed_chat_backfill_loopback`,
  `first_broadcast_after_join_loopback`, `loopback_room_split` (heal after a
  split), and `resend_dedup_loopback`. So it gives a late joiner the history and
  the shared meta document (the peer cards), it repairs a lost broadcast, and it
  heals a split.
- Cost: see the traffic numbers above. It runs every 10 s on every member, and
  an answer floods up to 64 frames mesh-wide.
- User messages are rare (7 chat messages and 493 directed frames in 83 min on a
  43 member gossip). A repair per user message (a sender retry after a missing
  receipt) costs O(N) per message. Anti-entropy costs O(N^2) every 10 s even
  when nobody sends anything.

What the proposal must cover (the advisor lists them): a late joiner gets the
meta document without a peer that retries; a sender that leaves before the
retry; a broadcast has no recipient list, so the sender needs a receipt from
each member; directed frames already have receipts (`delivered`, `read`) and the
tuning comment says anti-entropy does not heal them anyway.

Options to compare (cost, risk, mixed-version plan), in order of how much
behaviour they change:

- (b) keep the shared meta document repair through state heads (a small digest
  and a direct sync), and remove only the frame-log resend path. Most of the
  flood goes, and the late joiner still converges.
- (c) classic cheap anti-entropy: one random peer per round, unicast, ids or
  heads only, an answer only to that peer. No mesh-wide resend.
- (d) the user's sender retransmit for directed and broadcast frames, plus (b)
  or (c) for the shared document.
- (a) remove everything. Needs a different way for a late joiner to get the
  meta document.

### Advisor review (`ignite-bench`, read-only, nothing run)

Anti-entropy is three mechanisms, not one. [V] by code reading, file:line in the
advisor's report:

- M-LOG: the chat-log digest every tick, answered by a mesh-wide resend with a new
  nonce (`gossip/antientropy.rs:146`, `:243-334`).
- M-DOC: the state heads digest for the `meta` document (cards, status, graph
  membership), answered by a unicast to a linked asker, else a mesh-wide resend
  (`:386-430`, `:598-694`).
- M-PULL: event-driven pulls: the digest on the first link, the digest to each new
  peer, `ask_back` and the fast rounds. They are not driven by the tick.

On the advisor's own node (16:00 to 16:47) 99.7% of the logged inbound frames are
repeats (804 distinct frames in 6,870 copies, 8.5 copies per frame), and 23 frames
are user frames.

What breaks if the tick is off, per class:

- A chat broadcast is lost for a member that was frozen longer than the 15 s path
  idle, or that lost a link (tests `gossip_network.rs:2359`, `:2513`, `:2601`,
  `:2752`; engine `missed_chat_backfill_loopback`, `resend_dedup_loopback`).
- Presence: a missed "left" leaves a ghost until the 90 s alive timeout, and the
  peer then shows "went quiet" instead of a clean leave. [G]
- A directed frame lost on an open link is not backfilled
  (`test_directed_msg_backfills_to_its_addressee`). The sender already gets an
  error on a failed send and a receipt when it works.
- The meta document for a late joiner needs M-DOC or M-PULL: without the cards the
  joiner cannot address A2A calls (`a2a/node.rs:128-130`).
- Partition heal needs M-LOG and M-DOC. [G for chat, V by tests for documents]
- Late join: the first-link digest (M-PULL) already gives the meta document, so the
  tick is not needed for it. [G, test S2 below shows it]

Sender retransmit: a Swarm broadcast gives the sender no recipient list and no ack
(`iroh-gossip src/api.rs:186-188`). Receipts exist only for directed frames. A
retry per broadcast costs N-1 receipts and a new wire frame, and it still loses a
late joiner and any frame of a sender that left first (including its own "left"
presence). It is cheap for chat (about 7 x 42 = 300 receipts in 83 min, against
about 6,800 repeat frames in 47 min on one node), but it is the wrong tool for the
document.

Options and verdicts:

- (a) Remove everything: breaks the classes above, about 14 integration tests and
  30+ unit tests fail. Rejected, it violates "no regressions".
- (b) Remove M-LOG only: saves the presence loop (3,080 of 6,850 frames) but keeps
  the meta loop (3,747), the larger one. Rejected as stated.
- (c) Cheap anti-entropy: each tick, send the chat digest and the two heads
  digests by unicast to one random linked peer. The peer answers only to us, only
  by unicast, never a mesh-wide resend and never a new nonce. Keep M-PULL. Cost
  O(1) per node per tick instead of O(N) receptions times O(N) fan-out. The state
  path already works by unicast in 0.12.10. One check is open: whether a 0.12.10
  node answers a chat digest that arrives on the unicast plane (`recv.rs:725`).
  Mixed versions: new nodes send unicast and stop answering Swarm digests with
  Swarm resends. RECOMMENDED.
- (d) Sender retransmit for directed frames only (retry until `delivered`, bounded)
  on top of (c). Not a replacement for (c).

Two loop fixes the advisor adds, each a behaviour change gated on R4:

- Meta loop (M-DOC): `handle_state_digest` over-serves when the asker's heads hold
  a hash we do not have: `changes_since_not_by` ignores unknown heads and sends
  from the start, capped at 64 (`mesh_doc.rs:357-386`). If the two documents never
  converge, the answer repeats every round. Fix: answer with heads only when the
  asker's heads are not all held, or skip a resend to the same asker within T.
- Chat loop (M-LOG): remember (asker, dedup key) of the resends and skip a repeat
  within T. A second candidate cause is the log eviction at 1,000 frames: a
  requester that evicted a frame stops listing it while the answerer's window
  still covers it. [G]

The advisor's tests: every subprocess test sets `--antientropy-interval-secs 2`,
and two engine test files pin the tick to 1 h on purpose, so they test only M-PULL
and guard option (c). Some tests pass trivially without anti-entropy
(`gossip_network.rs:1390`, `:1913`, `:2410`, `:2686`, `:4172`), so they cannot
catch a removal.

### Experiment R5 (tick off), replaces the earlier plan

`--antientropy-interval-secs 86400` on every node (M-PULL stays on), private
loopback gossip, N=6, release build, quiet host. For each scenario record the
delivered set per node, the repeat count per frame, the census `idle_resent` and
`idle_external`, and the CPU. Run each with the tick at 10 s (control) and at 86400.

- S1: `SIGSTOP` 20 s while others send 5 chat, 1 directed and 1 meta merge. Expect
  the control to deliver all 7 within about 2 ticks, and 86400 to lose chat and the
  directed frame for the frozen node.
- S2: late join after 20 chat and 10 meta merges. Expect both runs to give the
  meta document (first-link digest). This shows the tick is not needed for it.
- S3: partition and heal (block 3 nodes from 3 for 60 s). Expect the control to
  converge in about 2 ticks, and 86400 to converge meta only.
- S4: the sender leaves while a member is frozen. Expect 86400 to lose the chat and
  the "left" presence: the case sender retransmit cannot fix.
- S5: mixed version, 3 nodes 0.12.10 and 3 nodes with option (c).
- S6: the storm check (R4) at N=12, 10 min idle after a late join. Expect the
  control to show frames received more than 5 times, and option (c) with the two
  fixes to show none.

Result: pending.

## Proposal: remove the chained encryption of messages

The user proposes to remove it. Which mechanism is meant is not fixed: the
engine has a per-author hash chain (`AppClass::CHAINED`: `seq` plus a link to the
previous frame, signed, `gossip/recv.rs:1046`; `messages/chain_ctx.rs`,
`messages/tests.rs:568`), sealing of directed frames to the receiver's X25519 key
(the `mesh-seal` extension), a signature on every frame, and the QUIC transport
encryption. The user chose: measure first, then decide. Status: no change and no
number yet.

Plan (no code change):

- Take the share of CPU by class from the symbolized profile of the live daemon
  (`wire-straw`): signature verify, hash chain and id hashing, QUIC AEAD, seal and
  open of directed frames, JSON parse, and syscalls.
- A micro-benchmark of verify, chain check and seal per frame, if the profile does
  not separate them.
- Decision rule: if the cost of the checks is a small share once the resend flood
  stops (M1), keep them. Each check runs per received frame, so a flood of
  duplicates multiplies it, and removing the duplicates is the cheaper fix.
- Security note: the hash chain and the signature give integrity and authorship;
  sealing gives confidentiality of tasks and replies. Removing any of them is a
  security change, and it needs the user's explicit decision with these costs
  written next to the CPU gain.

Result: pending.

## Complexity audit: what could go with little loss

The user asked which complexity returns little or no benefit. Two read-only
audits: the app (`layer-oval`, at `b588fc75`, done) and the engine (`tale-gorge`,
pending). Rule for every item: a verdict needs a proven benefit or the words
"none found".

### App (`crates/agent-gossip`, 34,350 lines), [V] unless marked

- Task state machine (`a2a/task_record.rs` 2,832 lines, `status_update_params.rs`
  824): keep. A beat goes out only for a live task (every 30 s), and the stall
  check runs only for a task in the map. An idle daemon with no task sends zero
  beats and zero stall checks, so it costs nothing on idle.
- QUIC tunnel (`tunnel/`, 2,549 lines): zero runtime cost, reached only by
  `a2a expose` and `connect` (`cli/a2a.rs:15`). Remove candidate. Needs the user's
  answer: is `a2a expose/connect` used?
- Localhost JSON-RPC binding (`--a2a-serve`, off by default; `served.rs` 481 plus
  `rpc_error.rs` 429): zero runtime cost. Remove candidate. Needs the user's
  answer: does any local client use it?
- MCP server (`mcp/`, 2,231 lines, 28 commits touch it): started only by the `mcp`
  subcommand. Keep if agents use it.
- `doctor` (639 lines), `plug`, `unplug`: on-demand CLI. [G] plug and unplug were
  not read.
- Hidden flags: 39 `hide = true` in `cli/args`, 24 in `tuning_opts.rs`. Test-only
  use is verified for `--antientropy-max-resend` and `--heal-interval-secs`; the
  rest were not checked one by one. Simplify, but only after a per-flag grep.
- Peer meta written by the app: the agent card (about 2 KB, `card.rs` 611 lines,
  written once at start) and 13 more fields. Readers verified: the peers rows of the
  IPC (`ipc_command.rs:94`) and `reader_asks.rs:91`. Not verified: who reads `cwd`,
  `host`, `card_error`. The static part of the card is 83% of the entry: simplify
  (see "Metadata traffic", idea 4).

Ranking by (cost + size) divided by benefit: 1 the static card, 2 the tunnel, 3 the
JSON-RPC binding, 4 hidden test-only flags, 5 MCP. Note: these removals shrink the
code, but none of them runs on an idle daemon, so none of them reduces the idle
CPU. The idle CPU is in the engine (resends and traffic).

Not done: live-log counters, the test suite, git history per feature (all files
were last touched on 2026-10-30 by one refactor commit).

### Engine (`habilis-network`, about 58,000 lines in the main crate)

Pending (`tale-gorge`). First question: which engine crates and modules the native
daemon uses at runtime, against those that serve other consumers only (the wasm
peer, the C ABI, netplay, webrtc).

## Ideas for the CPU cost

Ranked by expected CPU saved and by risk. The advisor attacked the earlier
list, and this is the result.

### M1. Stop the resend loop (H7)

- Idea: find the member that keeps asking for the same frames and why it never
  stores them. Candidates, each needing R4 before and after:
  - (i) remember (requester, dedup key) for the resends already sent and skip a
    repeat within a time T. It changes no wire format;
  - (ii) answer one requester with a unicast to it, not a mesh-wide broadcast
    with a new nonce;
  - (iii) do not offer frames older than the requester's horizon.
- Experiment: R4 with a counter for resends per requester and for duplicate
  frames received (I6). Then the change behind a const, A/B on R4 and R3.
  Check that a really missing frame is still repaired.
- Result: pending.

### M2. Backoff in the `iroh-gossip` refill loop (C3)

- Idea: after a refusal, back off exponentially, or stop until a neighbour down
  event elsewhere. This is a change in the `iroh-gossip` fork.
- Experiment: count neighbour requests sent and refused and timer fires (I7).
  A/B on R2 and R3.
- Result: pending.

### M3. Cap the old-frame backfill and the replay of old frames

- Idea: frames 3.5 days old come in every minute. Age-filter or cap what a
  digest answer offers (see M1, iii) and what a join replays (about 1,160
  presence frames in 5 min after a join).
- Experiment: count frames by age before and after, on R4.
- Result: pending.

### M4. `Swarm` to neighbour scope for digests only (idea 1, narrowed)

- Idea: after M1, test whether H1 is a measurable share. Digests only, never the
  alive beat.
- Why narrowed: the gate `link_len == roster_len` flips on every churn event.
  Alive beats in neighbour scope do not reach an unlinked member, so the sweep
  evicts live peers and flapping grows. A digest in neighbour scope reaches no
  answerer that is not linked.
- Experiment: only if R1 shows IHAVE fan-out as a real share. Paired A/B at
  N=12.
- Result: pending. Low expected gain now.

### M5. Reclaim ticker only when work is due

- Idea: the 400 ms tick wakes 2.5 times per second. Small gain. Last, and the
  rendezvous tests must cover it, because the cadence also keeps rival sheds
  out of lockstep (`event_loop.rs:1409-1416`).
- Experiment: wake counts and CPU of the tick body, then A/B on idle wakeups.
- Result: pending.

### Parked or dropped

- Lower the default `--max-peers` to 6 to 8: wrong direction now. It makes a
  roster above the cap normal for any gossip above 9 peers, which is the regime
  that churns. Do it only after M2 and a clean R2.
- Merge the three digests into one frame: changes the wire format, and a mixed
  version mesh then never repairs. It needs a version bump and a compatibility
  plan. The back-off half does not stop H7.
- Merge the 10 s and 15 s tickers: a gain of a few wakes per minute, and it
  loses the per-timer stall telemetry.
- Longer QUIC keep-alive, `getppid` with kqueue: no evidence that they matter.

## Instrumentation to add

Most of it lives in the engine repo (`habilis-network`), so each item needs an
engine change and a pin bump here.

- I1 (done in the leader's engine tree, being ported to `main`): split
  `idle_external` per select arm; `getrusage` CPU delta on the census line;
  wake counts of the ppid watch and the admission sampler.
- I6: a counter of duplicate frames received and of resends per requester.
- I7: `iroh-gossip` neighbour requests sent, refused and timers fired.
- I8: the host load average on the census line, so overload shows next to the
  counters.
- I2, I3: `iroh-gossip` and endpoint metric deltas on the census line.

## Open questions

- Which member keeps asking for the same frames, and why does it never store
  them? [G] The frames are older than its join horizon, or a filter drops them
  before the log.
- Does the cap matter by itself? R2 against R1 decides.
- The reported 10% on the user's machine: which machine, how many peers, which
  OS version, and which release?
- The "3 signed broadcasts per 10 s" figure: the advisor corrected the split.
  The alive beat goes out only when nothing else was sent in 30 s
  (`lifecycle/heartbeat.rs:22`). The 3 are the chat digest and the state
  digests, with linkstate every 15 s on top. [G] I1 decides the exact split.
