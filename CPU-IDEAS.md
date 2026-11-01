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

## Summary so far (read this first)

State of the investigation. Each line says how well it is proven.

1. [V] A calm private mesh is cheap. Release 0.12.10 and `b588fc75`, same host, CPU
   per daemon: 1 daemon 0.125%; K=2 0.08%, K=4 0.12%, K=8 0.30%, K=12 0.40% to
   0.48% (`wire-straw`, load 3 to 6, no stalled timers, fit exponent 0.98). So the
   idle upkeep (timers, keep-alives, pings) is small.
2. [V] The live gossip is far more expensive: 55% to 91% of a core per daemon on
   `nyc-macmini1`, 16% on the leader's host, over 100 times the calm mesh, with
   `idle_external` 11,400 per 10 s against 121 to 149. Size does not explain it:
   a linear extrapolation to 40 members gives about 1.5% [G].
3. [V] What the live daemons do: the wire carries about 1,000 times the bytes of
   the logged messages (5.8 MB per second), and the same frames come in again and
   again (30 repeats per frame in my log). 98% of the logged bytes are `meta`
   frames, 2% are tasks and replies, and chat is a few KB.
4. [V mechanism, G effect] Why: a digest answer re-floods up to 64 frames mesh-wide
   with a new nonce, 3 answerers per digest, every 10 s, and nothing remembers what
   was sent. A live test measured 13.1 resends per second from one member, within
   31% of the prediction. The dedup memory (2,000 keys) is shorter than the flood
   (2.4 s against a 10 s digest period), so repeats pass it again.
5. [V] Where the CPU goes in a live daemon (one profile, n=1): syscalls 32%,
   Automerge 16% (38% inclusive; 17.4% is a full-document JSON hydration run for
   every meta frame, `on_meta_applied`), signature verify 11% to 15%, QUIC 11%.
   Encryption (AEAD) is 1%.
6. [G] The cost grows with the age of the gossip: 45% of the state frames come from
   peers that left, and 14 of 45 entries of the meta document are dead peers. A
   test of this (E6 below) is queued.

7. [V] The cap matters, but only for a factor of 2 to 3. R2 (`lumen-gear`, engine
   `43dc4f6f`, 12 daemons, `--max-peers 8` so the roster is above the cap, 2 reps,
   no stalled lines outside pauses, load 2.6 to 7.0): CPU per daemon 1.81% and
   1.27% against 0.64% and 0.66% at cap 32 (2.0 to 2.8 times); bytes in 109 and 46
   KB per second against 9.0 and 7.0 (5 to 12 times); packets in 216 and 104 per
   second against 51 and 39. The engine counters did not move: `idle_external` 91
   (against 90), `idle_resent` 0.3 (against 0.3). Neighbour up and down events are
   small (0.04 and 0.03 per 10 s). [G] The extra traffic is inside the
   `iroh-gossip` layer (membership or shuffle, hypothesis C3); the spread between
   the two reps is large, so the typical value is not known. The cap alone does
   not reach the live cost (1% to 2% against 55%), so history, size or the resend
   loop multiplies it.
8. [V] Fresh meshes of 12 daemons never reproduce the resend loop: R1, R3 and R4
   show `idle_resent` near 0 (R3 mean 0.3, max 40; R4 late joiner: one census
   window of extra duplicates, back to baseline in 20 s). The loop needs history.

9. [V] Churn history alone does not start the resend loop, but it makes every wake
   dearer. `mist-frame`, engine `43dc4f6f`, release `mesh_peer_load`, local relay,
   8 survivors, then 9 batches of 4 short-lived members (36 dead) that send
   directed messages for 15 s and leave by SIGTERM or by SIGKILL; survivors idle
   10 min. Census gaps: none (max gap 10.01 s). The SIGTERM churn phase overlapped
   the suspected suspension window (14:44 to 15:04 local) and is voided; its idle
   window and the whole SIGKILL run count. Results per survivor per 10 s:
   - resends and duplicates: 0.0 in every slot of both variants during the 10 min
     idle; `idle_resent_top` empty; `idle_external` 67 and `gossip_rx` 28, the same
     as a fresh 8-daemon mesh. H7 is NOT reproduced in this setup (history of
     presence and directed frames only, about 8 min old, no broadcast chat, no
     state merges). During each batch join: duplicates 34 to 102, resends 3 to 17.
   - CPU: 42 ms before the churn (matches the fresh N=8 run), then 85 ms (SIGTERM)
     and 90 ms (SIGKILL) in the 10 min after, 2.0 to 2.2 times as much, with the
     same wake counts. It decays slowly (about 100 to 65 ms). So each wake costs
     more with history. [G] The per-tick work grows with the retained history
     (the message log, the documents, the tables of dead peers). A symbolized
     sample before and after names the function (task queued).
   - Wire after the churn: 6.7 KB per second in and 19 KB per second out.

10. [V] M2c in the live gossip (`wire-straw`, `nyc-macmini1`): app `b588fc75` with only
    the engine rev changed (a scratch edit of `Cargo.toml` and `Cargo.lock`, the
    binary says dirty): A = `f8af1e6f`, B = `e265eaba`. Two pairs, n=2: both members
    joined the live gossip together, 480 s window from t=120 s, load 5.9 to 8.1, no
    stalled lines, the 3 live daemons in the background used 188% and 210% of a core.
    - The cost it targets is gone: `doc_json` under `on_meta_applied` is 31% and
      14% of the busy samples in A and 0.0% (0 samples) in B.
    - User CPU falls 22% to 26% (`cpu_user_ms` 3,858 to 2,836 and 4,102 to 3,186 per
      10 s), but system CPU rises 21% to 28% (1,996 to 2,561 and 2,649 to 3,201). The
      net: 58.6% to 54.0% of a core (-7.8%) and 67.5% to 63.9% (-5.4%).
    - Duplicates dominate in both arms: `idle_duplicates` about 10,000 per 10 s, which
      is 82% to 85% of `idle_external`; `idle_resent` 129 to 221.
    - Not explained: B sent more bytes out than A in both 60 s windows (6.4 to 10.4
      and 4.8 to 13.1 MB per second out). [G] The freed CPU drains the send queue or
      the receive queue faster, so the daemon does more of the same work; the cause
      of the work (the duplicates) is unchanged. n=2 single windows.
    - Reading: M2c is real but small in net (5% to 8%) because the flood stays. A
      receive-side fix does not stop the flood from the other members. Only the
      senders stop it (M1 on every member), so M1 can only be tested in a private
      mesh in which every member runs the arm (E6b), not as one member in the
      live gossip. The receive-side fixes that cut the work per duplicate (M2b-1,
      M2d) can be tested one member at a time in the live gossip.

11. [V indicative, void by the load gate] History alone reproduces the live regime in a
    private mesh (E6 preliminary, `wire-straw`, app `b588fc75`, release, private
    gossip, 12 live survivors, 60 s settle, 120 s window after all short-lived
    members left, 0 stalled lines):

    | History | CPU per daemon | roster_len | link_len | idle_external per 10 s | idle_resent | meta doc | load |
    |---|---|---|---|---|---|---|---|
    | fresh | 0.52% | 11 | 11 | 35 | n/a | 24.6 KB | 5.6 to 6.5 |
    | 36 dead | 2.27% | 23.7 | n/a | 246 | 0.2 | 37 KB | 12.0 to 6.7 |
    | 60 dead | 18.36% | 44.8 | 10.0 | 3,449 | 22.5 | 70 KB | 20.6 to 31.0 |

    The two history runs are void under the gate of 9 (the load rose during them), so
    they are indicative only. Still: the cost and the external wakeups grow far
    faster than linear in the number of dead members (7 and 100 times), and the
    roster keeps the dead (11, 24 and 45 entries) while only 10 to 11 links exist.
    [G] Hypothesis H9: every dead roster entry keeps costing (re-dials, probes,
    PeerInfo or linkstate gossip about it), so the cost is about live members times
    dead entries. Not yet shown: whether the cost decays with time (is there a prune
    of dead peers, and after how long) or whether it is the load itself. Queued: the
    repeat on the instrumented build with a 15 min time series of roster_len,
    link_len and each `idle_ext_*` arm.

12. [V] The extra cost per wake with history has a name: the anti-entropy digest
    (`mist-frame`, engine `43dc4f6f` plus an uncommitted-then-committed example
    change, release build with symbols; 8 survivors, 9 batches of 4 members, each
    sending 60 chat messages and 5 state merges, so about 2,160 chat and 180 state
    frames, above the log of 1,000 and the seen set of 2,000; window 17:00 to 17:19
    local, no stalled lines, max census gap 10.55 s; load 11 to 26, above the gate
    in the idle window). Per survivor per 10 s: CPU 43 to 46 ms before, 272 ms during
    the churn, 84 ms in the 10 min idle after it; `idle_external` 68, 504 and 67;
    `idle_resent`, `idle_duplicates` and `idle_resend_dropped` 0 in every idle slot.
    A 30 s `sample` of the same survivor before and after (busy samples 147 and 231):
    - New cost after the churn: `bs58::decode_into` 27 and `bs58::encode_into` 14
      samples (none before). Callers: decode comes from `antientropy::handle_digest`
      (`decode_ids`), encode from `WireWindow::encode` under `broadcast_digest`.
    - Inclusive, before to after: `handle_digest` 2 to 29, `broadcast_digest` 0 to
      14, any "digest" 2 to 44, `ingest` 10 to 38. `MeshDoc`, `to_json`,
      `changes_since`, the sweep and the state-file refresh stay near 0.
    - So digest build and receive explain 43 of the 84 extra busy samples (the
      profile is a 30 s sample at 1 ms on a loaded host; the noise is not measured).
    [G, corrected: a digest holds at most 140 ids, see item 14] Each digest lists the
    ids of the log window as base58 text, so the cost scales with the log size and
    the number of peers at the same wake count, and the calls may come from more paths
    than the 10 s tick. H7 is again NOT
    reproduced, this time with history above both caps and for 10 min.
    Candidate fixes (not built): send the ids as raw bytes, or a hash or Bloom filter
    of the window, instead of base58 text; or decode only after the digest was
    admitted (check the order). A wire change needs the compatibility plan from the
    advisor review.
13. [G] New hypothesis H10: the resend loop needs MIXED VERSIONS. Every private mesh
    so far ran one build. The live gossip mixes releases (0.12.9, 0.12.10, main), and
    the advisor and `origin-magnet` found that a frame class can be retained by one
    version and not by another (`loggable` differs between versions, `recv.rs`
    `retained_after_lifecycle`). Test: a private mesh with 6 members of the installed
    0.12.10 and 6 of `b588fc75`, against the all-new mesh (0.40% to 0.48%).

14. [V] E7 corrects item 11: dead entries do NOT keep costing; the cost is a transient
    storm that ends, followed by a residual. (`wire-straw`, app `b588fc75` with the
    engine rev `f8af1e6f` (instrumented, rev edit only, dirty), private gossip, 12
    survivors, 2 runs with 60 dead members and fresh runs; 0 stalled lines.)
    - Storm: after the last member leaves, `idle_external` stays at 2,400 to 3,900 per
      10 s for about 1 to 3.5 min and then collapses to about 50 (run 1: 3,258 at 0 s,
      3,458 at 90 s, 1,454 at 180 s, 55 at 210 s; run 2: 2,443 at 0 s, 3,945 at 30 s,
      443 at 60 s, 48 at 90 s). The roster then prunes from about 44 to 11 entries at
      about 150 to 300 s. So a prune exists and works; the E6 window (60 to 180 s)
      fell inside the storm. The two runs differ by about 2.5 times in time.
    - In the storm: duplicates are about 2,800 of 3,050 external (about 90%);
      `gossip_rx` is the arm that grows (2,573 to 2,802), then `unicast` (244 to 467);
      `neighbor_up` about 1; `direct_rx` 11 to 12; no address or probe arm above 1. CPU
      per daemon 21% to 23% at t=30 s, host load up to 36, and the live daemons on the
      host stayed at 119% to 125% of a core.
    - Residual that does not decay in 15 min: 3.03% and 2.03% per daemon (mean for
      t >= 420 s) against 0.50% fresh, 4 to 6 times, with the same `gossip_rx` (36),
      resends 0 and duplicates 0. `cpu_user_ms` 140 to 230 per 10 s against 33;
      `idle_external` 43 to 50 against 36 (the extra is `direct_rx` 5 to 8 and
      `other` 2 to 5). So more CPU per wake, not more wakes. The meta document is 82.7
      KB and 64.0 KB against 24.6 KB and does not shrink after the prune. [G] The
      residual follows the size of the meta document or the history (work per frame or
      per tick); n=2. A residual profile is queued.
    - A private mesh reproduces a storm of about 3,000 external for a few minutes, not
      the live steady 12,000. [G] The live flood needs ongoing joins and leaves to
      restart the storm, or mixed versions (S5 queued).
    - Correction on item 12 (`mist-frame`): a digest carries at most 2 windows of 70
      ids, that is 140 ids, not about 1,000. Why digest encode and decode still show up
      at that size is under analysis.

15. [V] The digest cost is quadratic base58, small, and constant in N (`mist-frame`,
    micro-benchmark, scratch crate, bs58 0.5.1 against hex, release, single thread, 200
    rounds, 2 runs, load 17 to 11, noise 10% to 30%). Base58 of a packed window costs
    (encode, decode) in microseconds: 35 ids 520 and 300; 70 ids 2,200 and 1,500; 140
    ids 10,000 and 5,000-7,000; 1,000 ids 522,000 and 273,000. Hex needs 5 to 130
    microseconds. So 70 ids cost 4.4 times what 35 cost.
    - Order in `handle_digest` (verified, `antientropy.rs:243-270`): serde parse, then
      `answers_digest`, then `admit_digest` (5 s per author), then the base58 decode.
      So the decode runs only for the 3 answerers: about 3 digests of 2 windows per 10 s
      per node whatever N is. A node encodes 1 digest of 2 windows per 10 s. No other
      path encodes or decodes windows.
    - Cost per node per 10 s: encode 4.4 ms plus decode 9 ms, about 13 ms (0.13% of a
      core), which matches the sample (14 and 27 samples in 30 s). Before the churn
      the log held under 70 ids, so the quadratic term was small.
    - It does not grow with N, so it does not explain the live cost (55%). It is
      about half of the extra CPU per wake after a churn in a small mesh (84 to
      about 63 ms per 10 s at N=8, a guess).
    - Fixes: skip-decode is already in the code. Caching the encoded newest window saves
      at most one of two encodes (2.2 ms per 10 s), no wire change. Base64 ids in a new
      field `ids64` is linear (under 10 microseconds, a guess) and removes about 13 ms
      per 10 s, but it is a wire change: a 0.12.10 daemon fails the parse
      (`report_unreadable_digest`, no resends), so old and new nodes would stop
      repairing each other's chat gaps. Putting base64 in the old `ids` field must not
      be built (an old answerer would resend its whole budget for every digest).
    - Decision: PARK. Under my rule (no behaviour change without a measured gain that
      pays for it) a 0.13% gain per node does not pay for a wire change.

16. [V] The residual is named: `MeshDoc::holds_heads` under `handle_state_digest`
    (`wire-straw`, residual profile; app `b588fc75`, engine rev `f8af1e6f`, rev edit
    only; 12 survivors after a history of 60 dead members; two consecutive 30 s samples
    at t >= 420 s, load 6.0 to 3.8, no stalled lines; a fresh survivor sampled 2 min
    later, load 4.7 to 3.9). CPU then: 1.9% to 2.1% per daemon against 0.46% to 0.49%
    fresh; meta document 62.0 KB against 24.6 KB.
    - `holds_heads` is 234 and 240 of 535 and 524 busy samples inclusive (44% to 46%)
      against 28 of 92 (30%) in the fresh survivor: 8.5 times the absolute cost, on the
      same 36 `gossip_rx` per 10 s and a document 2.5 times bigger.
    - The code (`habilis-network-doc/src/mesh_doc.rs:391`): for each head of each
      incoming state digest, `decode_hash` and then `self.doc.get_change_by_hash`. The
      self frames under it are Automerge `ChangeCollector`, `op_iter` and `hexane`
      cursors, so [G] `get_change_by_hash` rebuilds the change from the op set (inlined;
      the frames show it, the name shows 0 samples).
    - The document already keeps `applied: HashSet<ChangeHash>` (line 152), used by
      `ingest` for the O(1) "already applied" check, and a test pins that a change
      recorded as applied is in the document (line 1137).
    - Not the cause: no per-tick walk of the document (`on_tick`, `state_refresh` and
      the state file have 0 samples in all three samples). `get_changes` 1 to 2 samples.
    - Other rows, h60 against fresh (inclusive): `sendmsg` 101 and 100 against 11 (not
      explained: 9 times more sends on the same `gossip_rx`; part may be the 5 to 8
      `direct_rx` exchanges), `bs58` 66 and 93 against 17 (12% to 18%), `handle_digest`
      53 and 84 against 13, ed25519 verify 20 and 16 against 5, `serde_json` 34 against
      1. Samples under 5% are noise (about 530 and 92 busy samples).
    - Idea M3 (not built): answer `holds_heads` from the `applied` set instead of
      `get_change_by_hash`, or skip it when the digest heads equal our own heads. No
      wire change; the result must be the same. A property test against
      `get_change_by_hash` on docs with merges, forks, pending orphans and a snapshot
      load, and a micro-benchmark on a 60 KB document, decide it.

17. [V] M3 on an aged document (`origin-magnet`, scratch test in the M3 worktree, release,
    load about 30 so read it as an order of magnitude, 2,000 calls per row, 3 heads
    asked): a gated meta-style document where every member writes 3 merges of a card
    of about 600 bytes, all without seeing each other, so the change graph is wide.

    | authors x merges | bytes | changes | old µs per call (3 heads) | old µs per `get_change_by_hash` | new µs per call |
    |---|---|---|---|---|---|
    | 1 x 3 | 306 | 4 | 5.9 | 5.9 | 0.77 |
    | 10 x 3 | 1,659 | 31 | 83.0 | 23.5 | 3.6 |
    | 30 x 3 | 4,085 | 91 | 190.8 | 60.8 | 2.6 |
    | 60 x 3 | 6,748 | 181 | 365.6 | 122.3 | 2.6 |
    | 120 x 3 | 11,772 | 361 | 809.2 | 268.1 | 2.6 |
    | 60 x 12 | 9,326 | 721 | 411.4 | 154.7 | 2.6 |

    The old cost grows with the number of changes (5.9 µs at 4 changes to 268 µs at
    361, about 0.7 µs per change): a scan, not a lookup. It grows more with the number
    of authors (the width of the graph) than with the rounds: 60 x 12 costs 155 µs
    per lookup and 120 x 3 costs 268 µs [G, from two rows]. The new cost is flat,
    about 2.6 µs for 3 heads, mostly the base58 decode. Gain per call: about 140 times
    with 60 authors and 310 times with 120. A digest asks this for each of its heads,
    so with 60 heads the old code pays up to 7.3 ms per digest. Not measured: the real
    62 KB document of the survivors and the share of busy samples after the change
    (that needs the aged-mesh residual test and the live A/B on `ab-stack2`).

18. [V by reading, G for the effect on a live mesh] A fast-round loop can keep a node
    sending for ever (`origin-magnet`, engine `ac08f822`, no code changed, not measured
    on a mesh). It answers why an aged survivor sends about 9 times more per received
    frame. File:line under `crates/habilis-network/src` unless noted.
    - What a node SENDS when a state or meta digest arrives (`gossip/recv.rs:775` and
      `:792` call `handle_state_digest`, `gossip/antientropy.rs:613`):
      - `ask_back` (`antientropy.rs:478`): if the digest heads are not all in our
        document, we note the sender as "ahead" (`fast_rounds.rs:130`) and, if allowed,
        send our own digest to it by unicast (`ask`, `:558`). The same peer may be asked
        again after 200 ms (`FAST_ROUND_MIN_INTERVAL`, `tuning.rs:1049`).
      - The answer (`:639-693`): if the asker lacks something we hold, we send up to 64
        frames plus a closing digest, by unicast to a linked neighbour, else by gossip
        broadcast with a new nonce. Gate (`serve_budget.rs:46`): per 5 s window up to 8
        answers per asker, channel and plane on unicast (1 on gossip), so one asker can
        draw up to 8 x 64 = 512 frames per 5 s per channel.
      - Own digests by the 10 s tick and on a new peer or first link: about 2 per 10 s.
      - A fast round that repeats by itself, with no digest arriving
        (`resume_fast_rounds`, `antientropy.rs:527`): called from the reclaim ticker (400
        ms, `daemon/event_loop.rs:1436`), it asks one "ahead" peer per channel when
        nothing was asked for 400 ms (`FAST_ROUND_RETRY`, `tuning.rs:1058`).
    - What can repeat for ever: the retry fed by `ask_back`. A peer stays "ahead" for 1
      min (`FAST_ROUND_AHEAD_TTL`, `tuning.rs:1065`) after its last digest, and every
      digest that still shows heads we lack (every 10 s) renews it. The retry forgets
      the peer only when we hold its heads. So while a peer advertises heads we can
      never hold, we send it a unicast digest every 400 to 800 ms per channel and it
      answers up to 64 frames each time: up to 5 sends per second, 50 per 10 s, which
      fits `direct_rx` 5 to 8 and `other` 2 to 5 and the 9 times `sendmsg` [G on the
      exact link].
    - Heads that never converge (a node is "ahead" only through its applied heads):
      a change the receiver refuses and the holder applied (the card-forgery gate,
      `Ingested::Rejected`, `recv.rs:1227`; a repeated actor and seq with other
      content, `mesh_doc.rs:501`), so the refused hash is never in `applied`; and the
      over-serve of unknown heads (`changes_since_not_by`, `mesh_doc.rs:371-385`,
      Automerge drops unknown hashes from `have`, so all unknown heads return the
      whole history cut to 64, the same first 64 frames every time, while the frames
      really missing come later in the order). A change whose dependency nobody holds
      waits in `pending` and is not in the heads, so it is not a source of the loop.
    - Growth: there is no GC or horizon in the document crate for the changes of
      departed members: `applied`, `frames` (a signed frame per change, kept for
      re-serve) and the Automerge history only grow. The engine never builds a change
      outside tests, so a delete of a departed member's entry is the app's job, and
      even then the tombstones stay [G]. The growth 24.6 KB to 62 to 83 KB fits that
      [G, not measured].
    - Not visible today: the send side. The census counts only receive-side wakes. Planned
      counters (census only): `idle_fr_asks` and `idle_fr_retries`, `idle_state_answers`
      and `idle_state_answer_frames` (split unicast and broadcast), `idle_ahead_peers` (a
      gauge: peers noted as ahead, summed over the two channels; above 0 for minutes on a
      mesh with no writes is the loop) and optionally `idle_state_rejected`.

19. [V] The refill backoff in the `iroh-gossip` fork cuts the cap-exceeded traffic by
    about 70% and CPU by 15% to 30%, but not down to the cap-32 level, and the heal
    check has an open case (`lumen-gear`, `nyc-macmini1`, harness `mesh_peer_load`, 12
    daemons, `--max-peers 8`, idle 10 min, last 5 min sampled, order A B A B, no
    stalled lines; arms A = `6a8ebd12`, B = `8c068b2f` with the fork at `fec3ef8`).
    Per daemon:

    | Run | CPU | packets in/out per s | KB per s in/out |
    |---|---|---|---|
    | A1 | 1.33% (load 30.4 at t=240 s, contaminated) | 116 / 119 | 50.5 / 50.4 |
    | B1 | 0.70% | 52 / 52 | 12.5 / 13.1 |
    | A2 | 1.04% | 106 / 108 | 45.3 / 45.5 |
    | B2 | 0.88% | 62 / 62 | 16.4 / 15.9 |

    Bytes in fall about 70% (A mean 47.9, B mean 14.5 KB per second) against 7 to 9 at
    cap 32; the clean CPU pair is 1.04% against 0.88%, against 0.64% to 0.66% at cap
    32. Settled counts are alike in all four runs (neighbour up 0.04 to 0.05, down 0.02
    to 0.03 per 10 s, `link_len` 7.3 to 7.6, roster 11, `idle_external` 91 to 93,
    resends 0.2 to 0.4). So the backoff removes most of the extra Neighbor and
    Disconnect traffic of a roster above the cap, as the patch intended.
    Heal check (SIGKILL one random joiner at 180 s, watched 90 s, 3 reps per arm; the
    per-second link count does not dip, so it was read from the logs): detection of
    the victim by its gossip neighbours takes 34 to 69 s in A (n=24, median about 44) and
    34 to 84 s in B (n=21, median 34.2), clustered at 34 and 54 s in both; replacement
    after the detection is mostly under 1 s in both. The slowest replacement is 88.5 s
    in both arms. In rep B-2 two survivors had no replacement by the end of the 90 s
    watch (the load was 11.5 at the kill and the victim had 5 neighbours), A had none:
    one open case, worth a repeat. Caveats: built with Homebrew rustc 1.99.1 (the
    pinned nightly is absent), local relay lookup, 2 and 3 reps.

20. [plan, relayed request 19:27 local] The goal of the study is to reduce the metadata on
    the wire. The ratio of metadata to data must be measured by frame kind, next to CPU,
    before and after each fix. Definitions used here: data = what users create (chat,
    directed messages, task status and artifact legs); metadata = everything else
    (state and meta change frames, presence, alive beats, PeerInfo, digests, linkstate,
    ping and pong, receipts, resends of any kind, and the per-frame overhead of
    signature, ids and nonce); transport overhead (QUIC, TLS, UDP, IP) is separate and
    measured by `nettop` against the sum of the frame bytes. Counters needed (engine
    census, per interval): bytes in by kind split into first sightings and duplicates,
    bytes out by kind, and body bytes against wire bytes. Task assigned to
    `origin-magnet` (branch `...-origin-magnet-m8`). Interim numbers from existing
    data: on my daemon (83 min) the logged inbound bytes are 98% `meta` state frames
    (15.66 MB), 2% directed (0.30 MB) and chat about 3 KB, while the wire carried 2.66 GB
    in; a live meta document is 104 KB for 45 peers, 87% of a peer entry is the
    static agent card, and the whole document compresses by 91% with gzip (items
    above). The full table by kind comes from the new counters.

Ranked fixes (none is done and measured yet):

- M1: remember what was resent to whom (prototype with red and green tests exists,
  off by default, not benchmarked).
- M2c: do not hydrate the whole meta document for every meta frame (up to 17% to 35%
  of the busy time).
- M2d: a longer dedup memory.
- M2b-1: check the dedup before the signature verify.
- Cheap unicast anti-entropy (option c) and removal of the state of dead peers.
- Not worth it: removing the encryption (1%); removing anti-entropy as a whole (it
  breaks 14 integration tests and the late-join and loss repair).

Open: R1 to R4 (`lumen-gear`), the instrumented bench (`mist-frame`), the M1 branch
(`origin-magnet`), E6.

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
- Another live member, on `nyc-macmini1` (`wire-straw`, release 0.12.10, 15:30,
  `nettop`): lifetime 56.1 GB in and 97.3 GB out over about 3.2 h; one 5 s window
  gave 4.7 MB per second in and 10.5 MB per second out. Out is about 1.7 times
  in, as on my daemon (2.2 times). `nettop` on that host gives bytes only, no
  packet column.
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

### Symbolized profile of a live daemon, [V]

By `wire-straw` on `nyc-macmini1`: a member built from `b588fc75` with symbols joined
the live gossip for 5 min. One 30 s `sample` at t=180 s, so n=1 and no noise figure.
Host load 5.0 to 5.3, no stalled-timer lines. The daemon used 18.35 s of CPU in 30 s
(61% of a core). Census over the 5 min: `idle_external` 11,404 per 10 s,
`idle_resent` 224, `link_len` 19.1, `roster_len` 38.3. The background CPU of the
other live daemons was not recorded. An earlier sample at t=20 s (join spike) gave
the same ranking.

Self time, as a share of 15,629 busy samples:

| Class | Share |
|---|---|
| `sendmsg` + `recvmsg` (syscalls) | 31.7% (`sendmsg` alone 27.9%) |
| automerge | 15.8% |
| ed25519 (curve25519 field ops) | 14.7% |
| QUIC state machine (`noq`) | 11.4% |
| alloc and memcpy | 6.1% |
| `iroh-gossip` | 4.7% |
| base58 | 4.5% |
| blake3 and sha | 1.6% |
| engine | 1.4% |
| tokio | 1.3% |
| QUIC AEAD | 1.0% |
| JSON | 0.5% |
| app | 0.1% |
| other | 5.0% |

Inclusive time (overlapping, so the rows do not add up): automerge 38.4%
(`mesh_doc` 2,751 samples), `serde_json` 21.7% (`mesh_doc` 2,713 and `messages` 565),
ed25519 verify 11.1% (all under `messages`), ed25519 sign 0%, X25519 seal and open
0.0%, `antientropy missing_frames` 2.8%, `handle_state_digest` 3.1%, hyparview 0.0%.
Caveats: hyparview near 0 can be inlining into the `iroh-gossip` actor, so it is not
proof that the refill loop (C3) is absent. [G]

What it means (the first two [V], the rest [G]):

- Per-message encryption is cheap: QUIC AEAD 1.0%, directed-frame seal 0.0%
  (there were almost no directed frames), blake3 and sha 1.6%. Signature verify
  costs 11% (inclusive) to 15% (self). So removing the encryption would save about
  1% of the busy time, and removing signatures at most 11% to 15%, and that is a
  security change.
- About 80% of the busy time sits in work that a duplicate frame triggers: syscalls
  (32%), the automerge and JSON path of the `meta` document (38% inclusive, which
  overlaps with the JSON share), and signature verify (11%).
- So the cheaper fix is to stop the duplicates (M1), and to drop an already seen
  frame before the heavy work: check the dedup key before the signature verify,
  and check that a state change is already applied before the JSON parse and the
  automerge apply (idea M2b below). Each one changes no wire format and no result.

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

## Target: each idle process below 1% CPU, in a mesh of 50 agents (set by the user, 22:10 and 22:15 local)

Definition. CPU of one daemon process = cputime delta divided by wall time, times 100
(100% is one core), over at least 10 min, with no user traffic. Report the median and the
worst daemon. The target passes when the worst daemon is below 1% in a mesh of 50 agents
(50 daemons, an aged meta document, the size of the live mesh) and in calm meshes of 12, 24 and 50
daemons, with no regression and no behaviour change that lacks a measured gain.

Worst daemon, from `mist-frame` (old runs on 43dc4f6f, before any fix; CPU = (user + sys) over wall
time, without the first 30 s, loads below 12; median / worst): N=2 0.17% / 0.17%; N=8 0.42% /
0.73%; N=12 0.67% / 1.05%; after churn, 10 min, 8 survivors: kill 0.83% / 1.27%, publish 0.80% /
1.12%. So the median is below 1% everywhere, but the worst daemon is above 1% already at N=12
and after churn.

Where we are (measured; the live figures are as recorded earlier in this file, and I read them
as one daemon, 🟡 80%):
- 1 daemon alone: 0.125%. Calm mesh of 12: 0.40% to 0.70% per daemon. Both are below 1%.
- Live mesh: 16% on the leader host and 55% to 91% on `nyc-macmini1`. That is 16 to 91 times the
  target. [V] These are per daemon (the live-gossip fact above says "of a core per daemon").
- New reading, 22:14 local, leader host: 10 daemons, 60 s window (cputime deltas), host load 36:
  median 38%, mean 37.9%, maximum 39%; all 10 are above 1%. Caveat: the mesh was not idle (the study
  traffic and the benches ran) and the host was loaded, so this is an upper reading of the live
  figure. It still shows that every daemon on the host is far above the target today.

Expectation at 50 agents, before any fix [G, a linear extrapolation of measured calm points]: the calm
curve is linear in the number of daemons (fit exponent 0.98: K=8 0.30%, K=12 0.40% to 0.48%). That is
about 0.035% per member, so a calm mesh of 50 would use about 1.7% to 2.0% per daemon, with no flood
and no history. So the target at 50 needs two things: the flood and history cost removed (the live
16 to 91 times), and the per-member upkeep roughly halved. The cap is not the lever for the second:
at cap 8 the CPU was higher than at cap 32 (R2: 1.27% to 1.81% against 0.64% to 0.66%; with the refill
backoff 0.88% against 0.64%), so lowering the cap is not a way to the target. The cost per member is
unnamed. Candidates are the digests (N frames per 10 s received and verified), the alive beats, the
per-link QUIC upkeep (up to 32 links), and the wakes per frame. The tick-off runs of `layer-oval` and
the 50-daemon profile of F6 will name it.

What the measured fixes give: M2c is -5% to -8% net (user CPU -22% to -26%, system CPU +21% to
+28%). The refill backoff is 1.04% to 0.88% in the lab (-15%). These are tens of percent. A
gap of 16 to 91 times needs the cause removed, not trimmed: the resend flood, the history storm
after a join, and the retry loop. Candidates: unicast digests with no mesh-wide resend (item 7),
a unicast join backfill, the entries of dead members removed, and the M5 backoff.

How the target is tested: every CPU run reports the median and the worst daemon against the
1% line. The live A/B (arms B4 against F6) says how far the built stack gets. The profile of F6
then names what remains, and each next step is chosen by its share of that remaining busy time.
A scaling curve of F6 at 12, 24 and 50 daemons shows where the 1% line breaks as the mesh grows.

## Tracking: metadata to data ratio, before and after (requested 22:20 local)

Data is what users create: the app tags `a2a_broadcast`, `a2a_msg`, `a2a_status`, `a2a_artifact`.
Everything else on the wire is metadata. The tags `a2a_req` and `a2a_resp` carry directed calls
that can hold user text or a protocol call; they count as metadata in the strict ratio and are
reported apart (`calls_*`, `meta_per_data_in_loose`). Tool: `scripts/perf/summarize.ts` on branch
`feat-reduce-resource-consumption-leader-ratio` (sha 307004f0), run on the output folder of a run. Fields,
per run, in and out: `data_*`, `calls_*`, `meta_share_*` (metadata bytes over wire bytes),
`meta_per_data_*` (metadata bytes per data byte), `udp_per_data_*` and `gossip_per_data_*` (the bytes
of the layers below per data byte). Evidence: a 2-node fixture with known tag bytes; the new fields
are absent in the old script and every value matches a hand calculation.

Limits. The census names the 6 largest tags per interval, so a small data tag can be missing from a
sum. In an idle mesh the data is near zero, so the ratio is undefined there ("no-data"); the idle
cost is then read as absolute bytes per second per daemon. For a defined ratio, every floor cell
adds a loaded phase, the last 5 min of rep 2: each daemon sends 1 broadcast of 200 bytes every
30 s. Both arms get the same load, so the ratio compares before and after.

Results table (filled when the runs arrive; one row per arm and cell):

| Arm | Cell | CPU worst / median | bytes per s per daemon in / out | meta share in | meta per data byte, wire | udp per data byte | D share |
|---|---|---|---|---|---|---|---|
| before (B4) | pending | | | | | | |
| after (F6) | pending | | | | | | |
| after (F7, unicast digests) | pending | | | | | | |

Earlier datapoint, before any fix (live daemon, 83 min; the data is the logged inbound directed
frames, 0.30 MB, plus chat about 3 KB; the wire figure is nettop, 2.66 GB in): about 8,900 wire bytes
in per data byte. Caveat: two different instruments, and the data is only what that daemon logged.

## First floor read: calm mesh, 12 daemons (`lumen-gear`, cell 1, 23:18 local)

Setup: example driver, 12 daemons, transports UDP and multihop (no gossip transport, so the gt
counters are 0), cap 8, no user traffic. Arms B4 (`d694bbcc`) and F6 (`2f2bce89`). Values are sums over
the 12 daemons per second, window 480 s to 780 s (rep 1), no stalled timers. The host was shared
with another bench and with builds, so the CPU is overlapped (an upper reading). The run
`c1-F6-2` is void (load above 12 from 23:10 to 23:12); it repeats.

| | B4 (before) | F6 (stack, no U1) |
|---|---|---|
| wire frames in | 59 KB/s (4.9 KB/s per daemon) | 59 KB/s |
| body in (payload of frames) | 35 KB/s (59%) | 35 KB/s |
| socket (UDP) bytes in / out | 138 / 146 KB/s | 217 / 248 KB/s |
| our frames as share of socket bytes in / out | 35.8% / 3.3% | 19.9% / 1.8% |
| repeats (dup, cdup, d_share) | 0 | 0 |
| gossip layer: control share in; IHave bytes | 21.2%; 4.0 MB | 22.3%; 3.9 MB |
| CPU per daemon, median / worst | 0.70% / 1.22% | 0.93% / 1.57% |

Share of the frame bytes received, by kind (both arms alike): chat digest 49%, linkstate 26% to 27%,
state digest 11%, meta digest 11%, presence 2%, peerinfo 1%, app 0, state and meta change frames 0.

What this says (each point is [V] from the counters unless marked):
1. In a calm mesh 100% of the frame bytes are metadata, and 71% of them are the three digests. The
   anti-entropy tick is the largest frame cost of an idle daemon, and it grows with the member count:
   a daemon receives one digest per member per tick. This is the same N-linear growth as the CPU curve.
2. Linkstate (26%) comes from multihop, which this driver has on and the real daemon has off, so
   it does not exist in the real policy. In the real policy the digests are a larger share.
3. The unicast digest form (U1, arm F7) sends one digest per tick to one peer. In a calm mesh it
   should remove most of the 71% and the per-digest verify and compare cost. This is the lever with
   the best fit to the data so far. F7 cells are queued to read it [G until then].
4. Our frames are only 35.8% (B4) and 19.9% (F6) of the UDP bytes in, and 3.3% of the bytes out.
   The rest is the gossip layer, QUIC and keep-alives. The bytes out understate the frames by the
   fan-out. So the wire cost below our frames is larger than our frames; P3 splits it by variant: IHave
   is 4 MB beside 19 MB of gossip data messages.
5. F6 shows 57% more UDP bytes in than B4 for the same frame bytes, and a higher CPU (0.93%
   against 0.70% median). The F6 fixes target the flood, so in a calm mesh they should do nothing.
   Two reps of overlapped runs cannot tell noise from effect; this is open, not a claim.
6. Early window: in rep 2 the UDP bytes in at 180 s to 480 s are about 3 times those at 480 s to 780 s
   in both arms, while the frame bytes are flat. So 3 min of settle is too short for the socket layer.
   The floor is read from the late window. The cause is not established (asked: relay and IP split,
   datagram counts, early against late).
7. Against the target: at cap 8 and 12 daemons the worst daemon is above 1% in every clean window of
   both arms (median below 1% in the late windows). This agrees with the old runs (worst 1.05% at N=12).

Early window and the short connections (`lumen-gear`, B4 rep 2, totals over 12 daemons, 300 s,
cap 8, so the roster of 12 is above the cap): UDP bytes in 153.5 MB early against 52.5 MB late; relay
bytes 60.6 against 10.8 MB; datagrams 464k against 211k. IP paths added 15,602 against 3,150; paths
removed about half of that. Gossip messages received: neighbor 3,854 against 772, shuffle_reply 3,753
against 692, disconnect 3,704 against 633, while \`gossip\` data messages and shuffle requests are flat.
So the early excess goes with short connections: each reply or neighbor message opens a connection
that a disconnect closes soon, and each connection costs a handshake and new paths [V counters, G
cause]. F6 shows the same shape and more of it in the late window (paths added 4,986 against 1,725;
shuffle_reply 940 against 370), so it is further from settled, or churns more; open. This is the
roster-above-cap regime. The real daemon has cap 32, and a mesh of 50 is above it too, so this churn
should exist at 50 [G]. The refill backoff of the gossip fork (item 1 of the duplicate steps) targets
exactly this, with -70% bytes in at cap 8 in the earlier A/B. Arms for it, with no new fork push:
F8 = F7 (U1) with the fork pinned at the refill commit (\`...-ab-full8\`, \`a9135a18\`), and F9 = F6 with
the same pin (\`...-ab-full9\`, \`21520872\`); the gm counters read zero in them. The wake counts of B4
and F6 are equal (40,812 and 40,734), so the wakes do not explain the CPU gap between them.

Loaded phase (B4, rep 2, 480 s to 780 s, 200 bytes per daemon per 30 s; data tag \`msg\`):
\`data_in\` 891,905 bytes, \`meta_per_data_in\` 18.5, \`udp_per_data_in\` 80.4, \`gossip_per_data_in\` 53.9; CPU
median 0.99%, worst 1.69% (overlapped). The F6 loaded values are void. Caveat: \`data\` counts the wire bytes of
the data frames, envelope included. If the payload is about a third of a data frame (200 of about
600 bytes), the metadata per payload byte is about 3 times larger, about 57 [G: from the frame size].

## Metadata classification and reduction ideas (requested 19:30 local; analysis only)

Goal given: make non-critical metadata the main target of the study. Method: classify each
field of the meta document and each frame kind as critical for the mesh to work or not,
then order the proposals by bytes saved per risk. Sizes are measured on the live meta
document (45 peers, mean entry 2,290 bytes; script `field-sizes.ts` in the session
scratchpad). "Critical" is what the mesh needs to connect, authenticate and route;
"used by workflows" is what the gossip skills read (graph-up and status read `model`,
`host`, `instance`, `hash`, `node` and `status`; verified by reading the skill text).

### Fields of one peer entry (mean bytes per peer)

| Field | Bytes | Class | Why |
|---|---|---|---|
| card.endpoint (id, addresses) | 122 | critical | `adopt_meta_endpoint` reads it to dial a peer before its PeerInfo arrives |
| card extension `mesh-seal` params (X25519 key) | part of 354 | critical | directed frames are sealed to this key; without the card "no peer can send to us" (`a2a/node.rs:128-130`) |
| card.name | 21 | critical (small) | nickname binding |
| card.supportedInterfaces (url) | 191 | needed for A2A calls, mostly static | the url holds the peer id |
| card.version | 42 | useful for compatibility | keep |
| card extensions: prose descriptions of 5 URIs | about 1,000 of 1,325 | non-critical, static | `mesh-seal` 354, `mesh-blob` 289, `mesh-a2a-rpc` 277, `mesh-broadcast` 216, `mesh-state` 189 bytes each, mostly description text |
| card.skills | 240 | non-critical, static | same for every peer |
| card.description | 107 | non-critical | text with the nickname |
| card.defaultInputModes, defaultOutputModes | 163 | non-critical, static | |
| card.capabilities flags (3) | 107 | non-critical, static | |
| status | 16 | used by workflows | availability for delegation (`idle`, `available`, `busy`) |
| model, harness, host | 71 | used by workflows | graph-up picks peers by `model`; compares `host` for graph copy |
| cwd | 58 | non-critical label | shown in status; also a privacy cost |
| instance, hash, node, graph, joined_at, invited_by | 78 in 19 of 45 peers | used by workflows | only for graph members; `joined_at` orders peers |

Result: of 2,290 bytes per entry, about 1,860 are the card and about 1,700 of those are
static and identical across peers; only about 420 bytes (endpoint, key, name, version) are
unique per peer, plus the labels. The critical bytes are well under 25% of the entry.

### Frame kinds

| Kind | Needed for the mesh? | Note |
|---|---|---|
| State and meta change frames | yes (cards, status) | the content is the problem (static card), not the frame |
| Presence joined and left | yes for a clean roster | cosmetic if missed (ghost until the 90 s alive timeout) |
| Alive beats | yes, only when a node is silent | already sent only if nothing else was sent in 30 s |
| PeerInfo | yes for connectivity | [G] duplicates the card endpoint; the card hint exists to dial before PeerInfo arrives |
| Digests (chat, state, meta) | only for repair | cheap by themselves (0.13% of a core); the cost is the resend after them |
| Linkstate | only with multihop | every 15 s, sent on change |
| Ping and pong | no (on demand) | |
| Receipts | for delivery proof of directed frames | small |
| Resends | no in steady state | the flood (items 1 to 4 above); the fixes target it |
| Signature, ids, nonce per frame | signature yes (authenticity) | overhead measured by the new body-against-wire counters |

### Proposals, ordered by bytes saved per risk (estimates from the table; not built)

1. Stop sending the prose of the five card extensions: send the URI and the params only.
   Saves about 1,000 bytes per entry (44%). Risk low: the text is static, and no code
   found reads the description. Check first that an old receiver's card validation
   (`card.rs`) accepts a card without them; a mixed-version plan is needed.
2. Send the card by reference: the wire carries the 420 unique bytes and a template
   version; the receiver rebuilds the static part from the version it knows, with a
   full-card fallback for an unknown version. Saves about 1,700 bytes per entry (74%).
   Risk medium: an app-level wire change with a version bump.
3. Compress frame bodies: 48% on one entry and 91% on the whole document (gzip).
   Independent of 1 and 2, but 1 and 2 shrink what is left to compress. Risk medium (a
   wire change, CPU on both sides).
4. Remove the entries and old changes of departed members: 30% of the document is dead
   peers, and the history only grows. Risk medium: history of left peers disappears
   from `status`; the doc crate has no GC.
5. Send on demand what only workflows read: `cwd` (58 bytes, also a privacy cost) and
   the graph fields could be fetched with `mesh/meta.get` when a skill needs them, not
   replicated to every peer. Saves about 100 to 150 bytes per entry; risk medium
   (skills must call for it).
6. One unicast join backfill instead of floods: bytes received in the first minutes after
   a join. Risk low to medium.
7. Make digests unicast to one random peer (option (c) of the anti-entropy review) and
   stop mesh-wide resends: the largest saving of repeated bytes (items 1 to 4 above).
8. Drop one of two redundant endpoint carriers (card hint or PeerInfo) once the
   per-kind counters show their bytes [G].

What stays: endpoint, key, name, version, status, signature. The per-kind counters (item
20) turn these estimates into measured shares.

### Item 7 explained: unicast digests, no mesh-wide resend (requested 20:02 local)

How it works today [V by code reading and the live flood test]:
1. Every 10 s each node broadcasts a digest to the whole mesh: the heads of the chat log
   and of the state documents, at most 140 ids.
2. A member that holds frames the sender lacks answers. Up to 3 members answer per digest.
   A 5 s cooldown per author limits each answerer.
3. An answer resends up to 64 frames. It re-broadcasts them to the whole mesh with a new
   nonce (`copy_with_resend_nonce`). The new nonce makes the frame look new, so the dedup
   set does not drop it.
4. Every member receives the copy. Each one verifies the signature, then finds that it
   already holds the content. That is the repeat cost.
5. A peer that is "ahead" of the sender also triggers fast rounds: a 400 ms retry that
   renews itself with each digest and can loop for ever (item 16 to 19).

Measured cost today (live daemon, 45 members): 13.1 resent frames per second per member,
mean 47.7 frames per answer, cap 64 reached. On my daemon 98% of the logged inbound bytes
were state frames, and the wire carried 2.66 GB in 83 min (about 530 KB/s).

The unicast form, step by step [design, not built]:
1. A node sends its digest to one random peer each tick, not to the whole mesh.
2. That peer compares the digest with its own heads. If it holds more, it sends the missing
   frames to the asker only, on a unicast stream, with the original nonce.
3. If the asker still lacks frames, it asks again at the next tick or at once if the answer
   was full. The retry has a cap and a backoff (the M5 mechanism).
4. No re-broadcast. One copy of each missing frame crosses the wire, to the node that lacks it.

Expected bytes [G, not measured]: a repair that today costs up to N copies of 64 frames
(one per member) would cost 1 copy to the asker. In a calm 45-member mesh the repeats should
fall to near zero, as the heads match. The paired A/B (arms B0 and F2, `wire census`) will
give the number. Until then, treat any figure as a guess.

What it still guarantees:
- Late joiner: the first-link pull stays (M-PULL), and the digest finds what is left.
- Partition heal: when two sides meet, the first digest to a peer of the other side
  finds the difference, and the answer is a unicast backfill.
- Quiet peer return: the next tick pulls what it missed, one peer at a time, with no flood.

What it gives up:
- Speed of repair for a gap that many members share: one random peer per tick, so
  convergence takes more ticks. A node that misses a frame heals in about
  (peers holding it)^-1 ticks, not in one broadcast round.
- The chance that a frame reaches a node that never sent a digest. A digest is the only
  way to ask, so a node that is silent on digests is repaired by the others' digests only
  if the answerer pushes to it. The first-link pull covers a new node, and the node's own
  digest covers the rest.
- Old peers still broadcast digests and expect a broadcast answer. A mixed mesh keeps the
  old cost until all nodes upgrade; new nodes can answer an old digest by unicast.

Relation to the pending anti-entropy decision: it is the "cheaper form" that the advisor
review recommended (option (c)), not the removal. It keeps the repair and removes the
flood, so the decision to keep or remove the whole mechanism stays open and independent.
If the user chooses removal, item 7 is moot. If the user chooses to keep state repair
only (see the reassessment above), item 7 is the form of that repair.

Risk: medium. It changes who answers and how. The risks are slower convergence, a
retry loop (the M5 cap covers it), and the mixed-version mesh.

Test plan:
1. Unit tests: the answer goes to the asker only, with the original nonce; no
   re-broadcast; the 64 cap and the cooldown still hold.
2. Loopback tests that must stay green: late join, partition heal, a member frozen 75 s
   and 150 s, a sender that leaves, sender and receiver on old and new versions (S5).
3. Convergence time: time to heal a gap with 12 daemons, old against new, in the heal
   check of `lumen-gear` (the same cases as the refill backoff A/B).
4. Cost: repeats, bytes and CPU per kind from the `wire census`, B0 against F2 and then
   against the new arm, in the calm and the live mesh.
5. The red test for each guarantee above comes first, as the rules require.

## Target: zero non-critical bytes in a calm mesh (relayed 20:03 local)

Target as I read it: in steady state a calm mesh carries no non-critical byte. That means no
repeated frame, no static card, no label that the mesh does not need. It does not mean zero
bytes: liveness, identity and signatures are critical, so a floor remains. If the user
meant literal zero bytes, that is not reachable and a peer would look dead after 90 s.

Floor per source in a calm mesh (G unless a counter is named; the `wire census` and the
layer counters P1 and P3 turn each row into a number):

| Source | Can reach zero? | Floor left |
|---|---|---|
| Resent and repeated frames | yes | 0 after the loop fixes (M2b to M6) and unicast digests |
| Static card (prose, skills, modes, flags) | yes in steady state; once per join today | 0 with the prose removed and the card by reference; the unique 420 bytes per peer stay, sent once per join |
| Labels (cwd, host, harness, model) | yes | 0 if fetched on demand; `status` stays |
| Join backfill of history and dead members | yes | 0 after the entries of left peers are removed |
| Alive beats | no | 1 frame per node per 30 s, only when the node sent nothing else |
| Digests (repair) | no, unless repair is removed | 1 small frame per node per tick to one peer; can ride on the alive beat |
| PeerInfo, linkstate, ping and pong | to be measured | linkstate only with multihop; the others on change or on demand |
| Envelope of each frame (JSON, hex keys, signature) | no | about 300 bytes per frame; a binary encoding would halve it (wire change) |
| QUIC keepalive and ACK, gossip control (HyParView, IHave, Shuffle) | no | below our frames; P1 and P3 measure it |

Closest steps, in order of distance to zero (each one is measured before the next):
1. Counters first: the `wire census` (done), P1 endpoint bytes (next), P3 gossip bytes by
   message type. They give the floor and the ratio.
2. Remove repeats: M2c, M3, M5, M6 (built) and the unicast digests (item 7).
3. Remove the card prose, then send the card by reference (proposals 1 and 2).
4. Fetch labels and graph fields on demand (proposal 5).
5. Remove the entries and history of left peers (proposal 4).
6. Shrink what stays: compress, then a binary envelope (proposal 3 and a wire change).

The result to report with each step is the floor: bytes per second in a calm mesh of
12 daemons, split into data, critical metadata, non-critical metadata and the layers
below, and the ratio, next to CPU.

## Requirement: less duplicated data (relayed 20:05 local)

Added as an explicit requirement, next to the CPU and the metadata ratio.

Metric D: duplicate bytes received (the second and later copy of content that the node
already holds) as a share of all bytes received, per frame kind, before and after each fix.

What the counters read today (corrected 20:46 by `origin-magnet`; my first text was wrong):
- The dedup key is SHA-256(author key + message id). The resend nonce is a trailer outside
  the signed message, so a resend keeps its id and its dedup key. An anti-entropy resend of a
  frame the node still remembers is caught by the id dedup and counted in `idle_bin_dup`.
  Plumtree names a message by its bytes, nonce included, so each resend is a new gossip
  message and reaches our ingest. That is why the resend flood costs the receive path.
- Frames that slip past the id dedup: those whose key left the `seen` set (a FIFO of 2,000
  keys; 64,000 in the experiment arms), and a frame that carries the same change under
  another id. For chat the log (1,000 messages) is a second id source, so a chat resend is
  still a duplicate. Only the state and meta channels have a second check, the change hash.
- `idle_bin_cdup` (M10): bytes by kind of a frame that is new by id and whose change the
  document had applied. It is part of `idle_bin_fresh`, so a reader adds it to the repeats.
  For chat and app frames it is 0 by construction. `summarize.ts` reports
  d_share = (dup + cdup) / (fresh + dup), in total and by kind.

Sources of duplicates, with the evidence we hold:

| Source | Evidence | Can reach zero? |
|---|---|---|
| Anti-entropy resend flood (new nonce) | 13.1 frames/s/member live; 80% of busy time follows a duplicate | yes, in a calm mesh |
| Fast-round retry loop | `origin-magnet` M5: loop shown; backoff built | yes, after the cap |
| Fan-out copies (one broadcast over several links) | refill backoff A/B (`lumen-gear`, 12 daemons, cap 8): bytes in -70% | no: a tree needs a floor of copies; IHave and Graft add control bytes (P3 reads them) |
| Join backfill of the whole history | late join and churn runs; `holds_heads` cost | to one copy per joiner with a unicast backfill |
| Repeated state frames (the same change over many paths) | 98% of logged inbound bytes are state frames | to 1 copy per node, in steady state |

Floor for D: 0% from resends, join backfills and repeated state frames in a calm mesh. The
fan-out copies of a broadcast keep a floor above 0 until the gossip layer shows an eager tree
with no cycles; the floor is read from P3 (Gossip bytes out per frame) and the `wire census`.
I do not give a number before the counters read it.

Steps to cut duplicates, ordered by bytes saved per risk (estimates are guesses; the A/B reads them):
1. Refill backoff in the gossip fork: measured -70% bytes in on 12 daemons; the heal check
   has one open case (rep B-2). Risk low to medium.
2. Backoff on the stalled fast-round retry (M5): stops the loop; risk low.
3. Unicast digests, answer to the asker only, no re-broadcast (item 7): removes the resend
   flood; risk medium.
4. A unicast join backfill: one copy per joiner; risk low to medium.
5. Smaller duplicates: remove card prose, card by reference, compression (proposals 1 to 3
   of the classification): shrinks each copy that stays; risk low to medium.
6. Remove dead members' entries (proposal 4): fewer bytes in every backfill; risk medium.
7. Tune the eager tree (the active view size and the prune rules): the fan-out floor; needs
   P3 first; risk higher, as it changes gossip behaviour.

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

### Write-up: what removing anti-entropy means (requested 19:21 local)

Status: nothing is removed. This section answers "what depends on it, what breaks, what
the cheaper form keeps, a recommendation, and what the tests must prove". Sources: the
advisor design review (code reading, nothing run), `layer-oval`'s first tick-off round
(n=1 per cell), and the measurements above.

What depends on it (three mechanisms, see the advisor review above):
- Convergence after a partition: the chat-log digest (M-LOG) and the state heads digest
  (M-DOC) are the only way two halves exchange what each said during the split.
- Late joiners: the meta document (peer cards, status, graph membership) arrives
  through the first-link and new-peer digests (M-PULL), not through the periodic tick.
  Chat from before the join is not backfilled in any case (the digest floor is the join
  time).
- Lost frames: a chat broadcast missed by a member whose link dropped or that was
  frozen longer than the path idle (15 s); a directed frame lost on an open link.
- Peer return after quiet: a missed "left" presence leaves a ghost until the 90 s alive
  timeout, and the peer then shows "went quiet" instead of a clean leave.

What breaks or degrades if the whole mechanism goes: about 14 integration tests and
more than 30 unit tests fail (list in the advisor review); the late-joiner, resend-after-
loss, missed-chat, partition-heal and first-broadcast-after-join cases lose their
repair path. If only the periodic tick goes and the event-driven pulls stay (M-PULL),
the first tick-off round (n=1) lost nothing in a 20 s freeze, a late join and a sender
leaving; a 20 s SIGSTOP is too weak a loss (packets queue in the socket buffer), so
round two uses 75 s and 150 s freezes with 5 repeats per cell (pending).

What the cheaper unicast form keeps (option (c) of the review): every repair class
above, because each tick still sends the digests, but to ONE random linked peer by
unicast, answered only to that peer, with no mesh-wide resend and no new nonce. The
cost per node per tick falls from O(N) receptions times O(N) fan-out to O(1). The
price is slower repair of a gap, about log N ticks instead of 1 (a guess), and a
mixed-version rule (new nodes stop answering Swarm digests with Swarm resends).

Recommendation: do NOT remove it. Replace the mesh-wide resend (not the repair) with
the unicast form, add the resend memory and the two loop fixes the advisor lists, keep
the event-driven pulls, and add the sender retry only for directed frames (receipts
already exist). Reasons: removal breaks the late-joiner and partition repair that the
shared meta document needs; the measured cost is in the mesh-wide resend and the
history work, not in the repair itself; the fixes built so far (items 10 to 19) go after
those costs without a wire change.

What the tests must prove (before any removal or replacement):
1. The six named loopback tests (late joiner backfill, resend after loss, missed chat
   backfill, first broadcast after join, room split, resend dedup) still pass, and
   `gossip_network.rs` reliability tests pass (alternating baseline and candidate;
   `mist-frame`'s gate is running).
2. Freeze 75 s and 150 s (sends during the freeze) with the tick at 10 s against 86400
   s: delivered set per node, 5 repeats per cell; late join; sender leaves while a
   member is frozen; partition and heal (needs two relays or a packet filter).
3. A mixed-version mesh (0.12.10 and the candidate) repairs both ways and shows no
   frame received more than 5 times and no "digest could not be read" lines.
4. The cost side: the same runs report `idle_resent`, `idle_duplicates`, bytes and CPU,
   so a loss of repair is paid for by a measured gain.

### Reassessment: anti-entropy for state only (requested 19:24 local)

Position given: anti-entropy is not needed for messages; it is needed only for state
changes (the meta document), because state is event based and a snapshot is built from
the events. Scope of this section: analysis only, nothing is implemented before the
performance study is done.

1. Does it also cover messages, receipts and presence today, and what does it cost?
   [V by code reading, advisor review]
   - Chat-log digest (M-LOG, `gossip/antientropy.rs:146`, `:243-334`): the frame log
     holds chat broadcasts, `msg`, task status and artifact legs (app
     `a2a/node.rs:523-526`) and joined/left presence (`gossip/recv.rs:1691`). Not
     logged: alive beats, PeerInfo, digests, A2A request and response, receipts. So
     receipts are not covered; directed frames are backfilled only to their addressee
     (`daemon/message_log.rs:377-395`).
   - State heads digests (M-DOC, `:386-430`, `:598-694`) cover the meta document and any
     state channel. Event-driven pulls (M-PULL: first link, new peer, `ask_back`, fast
     rounds) are separate and not driven by the tick.
   - Cost, measured: of the logged repeated frames on the advisor's node (6,850), 3,080
     are presence (45%) and 3,747 are state (55%); on mine, 6,140 presence (44%) and
     7,883 state (56%). 23 user frames in 47 min. So the log path (presence and chat)
     is about 45% of the repeats and the state path about 55%. The chat digest itself
     is cheap (about 13 ms per 10 s, 0.13% of a core, constant in N); the cost is the
     mesh-wide resend (up to 64 frames per answer, 3 answerers) that follows it.
2. Can it be restricted to state, or replaced by a snapshot-from-events sync?
   - Restriction: yes, in principle, and it is small: the 10 s tick sends the chat
     digest and the state digests separately (`daemon/event_loop.rs:640-661`), so one
     const can skip the chat digest only, with answering left in place for old peers.
     The state path already has its own repair (heads digest, unicast ask, fast rounds)
     and does not need the chat log.
   - Snapshot sync: the document is built from signed change frames, and the forgery
     gate and the per-change signature check need each frame (`frames`, one per change,
     `mesh_doc.rs:153`). A plain Automerge snapshot would drop those signatures, so a
     late joiner must still receive the signed change frames. That is what the answer
     of `handle_state_digest` does now (up to 64 per answer; the unicast path with
     fast rounds). A single unicast transfer of all frames to a late joiner is a
     reasonable replacement for the many 64-frame rounds, but it still carries the
     frames, so it is an optimisation of the round trips, not a snapshot.
   - What must stay: the state repair after a partition (heads digest and its answer)
     and the first-link pull for a late joiner; the loop fixes of items 16 to 19 still
     apply to it (heads that never converge, the retry loop, the over-serve).
3. What is lost for messages if they are best-effort only? [V mechanism, G effect]
   - A chat broadcast is lost for a member that was frozen longer than the path idle
     (15 s) or whose link dropped when it was sent. A directed frame lost on an open
     link is not backfilled; the sender still gets an error for a failed send and a
     receipt (`delivered`, `read`) when it works, so a bounded sender retry until
     `delivered` closes that gap for directed messages and tasks.
   - Presence: a missed "left" leaves a ghost until the 90 s alive timeout and then
     shows "went quiet" instead of a clean leave; a missed "joined" shows the peer when
     its alive beat or PeerInfo arrives. Cosmetic and bounded.
   - Task beats keep tasks alive without the log, so a lost status leg shows as a stale
     state, not a timeout [G].
   - Evidence so far: the first tick-off round (n=1) lost nothing in a 20 s freeze, a late
     join and a sender leaving, but a 20 s SIGSTOP is too weak; round two (75 s and 150 s
     freezes, 5 repeats, path check) is pending.
   - The product decision: is a missed broadcast chat for a sleeping or frozen member
     acceptable? Agents mostly talk by directed tasks with receipts, but a human chat
     broadcast would be lost for that member.
4. Updated recommendation and test plan.
   - Recommendation: a staged path. Stage 1: keep anti-entropy for state only, and make
     the log path best-effort by skipping the chat digest tick and the mesh-wide resend
     of chat and presence (behind a const, off by default, so the A/B uses one binary),
     keeping the answering of old peers' chat digests unchanged. Add the sender retry
     for directed frames. Stage 2, only if stage 1 shows a gain and the tests pass:
     remove the log repair code. This removes the 45% log share of the repeats without
     touching the state repair that the late-joiner and partition cases need.
   - Mixed versions: old daemons keep sending chat digests; new ones answer them or
     ignore them (a choice for the test), and old nodes lose one answerer at most.
   - Tests that must prove it: (a) state convergence with the chat digest off: late
     join gets the meta document and the peer cards, partition and heal converge the
     meta document, a member frozen 75 s and 150 s while a meta merge happens, sender
     leaves; (b) message semantics under best-effort: directed message and task with
     receipt and sender retry survive a 75 s freeze, broadcast chat loss is recorded
     as the documented behaviour, presence ghost timing; (c) the existing loopback
     tests that name chat backfill are expected to FAIL or to be rewritten: list them
     (`missed_chat_backfill_loopback`, `resend_dedup_loopback` chat cases,
     `test_anti_entropy_set_convergence` and the others in the advisor review), so the
     change is deliberate; (d) the cost: resends, duplicates, bytes and CPU in the
     paired A/B and the live A/B (items 10 to 19), against the same run with the chat
     digest on.

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

Result of the first measurement (see "Symbolized profile of a live daemon"): QUIC
AEAD 1.0%, seal 0.0%, hashes 1.6%, signature verify 11% to 15% of the busy time.
So the encryption is not a cost worth removing. The verify is a cost, but it is
paid again for every duplicate. Not measured separately: the hash chain check
itself (no symbol of its own).

Plan (no code change):

- Take the share of CPU by class from the symbolized profile of the live daemon
  (`wire-straw`): signature verify, hash chain and id hashing, QUIC AEAD, seal and
  open of directed frames, JSON parse, and syscalls. Done above.
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

### Engine (`habilis-network` at `f7cd9bb`, about 58,000 lines in the main crate)

Audit by `tale-gorge`, read-only, no builds. Evidence: source counts, the engine
`Cargo.toml`, the app imports, and 42 live daemon logs (7,752 census lines).
Correction of an earlier report: `f7cd9bb` is checked out, and the old line
numbers came from `08b7615`.

What the native daemon uses [V for the app source, G for the build]: the app
depends on the engine with the feature `blob` only (`crates/agent-gossip/Cargo.toml:59`).
Engine defaults are `host`, `mdns`, `dht`. The app imports `protocol`, `embed`,
`runtime`, `ops` (8 calls to `ops::blob`), `net`, `util` and `iroh`. It does not
import `netplay`, `webrtc`, `multihop`, the wasm peer or the C ABI. Whether the
engine compiles them in anyway is not checked.

Live logs (default level): `heal tick` 11,875 lines, `reclaim tick` 11,263,
neighbour up 15,818 and down 15,104, linkstate 7,710, beacon 3,283, rival probe
1,142, `maintenance timer stalled` 99. Zero hits for cohost, multihop, fast round,
admission, shard, chunk, chain and `anti-entropy resent`. Zero is weak evidence:
those lines are probably below the default level.

Candidates, ranked by (cost + size) / benefit:

1. Anti-entropy frame-log resend (`gossip/antientropy.rs` 2,115 lines plus 307):
   SIMPLIFY. Remove the mesh-wide resend with a new nonce, keep the state heads
   repair of the `meta` channel. Test first: R4 and the six loopback tests. This
   agrees with the advisor's option (c).
2. Multihop (6,603 lines, optional feature, not default) and WebRTC (10,087 lines,
   `transport/webrtc.rs` 2,978): REMOVE from the native build, if the build
   compiles them. A size and build-time gain only, no CPU gain on an idle daemon.
   [G] to check.
3. Reclaim tick gate (400 ms, 2.5 wakes per second): SIMPLIFY, with the rendezvous
   tests. Small gain.
4. Admission sampler (`transport/admission.rs` 1,767 lines, 5 s): SIMPLIFY or KEEP
   after a test that the connection ceiling still holds.
5. `cohost_arm` (2,262 lines): KEEP until a single-host run shows it is not needed.
6. Netplay (12,931 lines): outside the daemon; move to a separate crate if it is
   only for examples. Not checked whether the main crate depends on it.

Keep: heal (15 s), linkstate (only on change), blob and chunks (the app uses
them), the per-author hash chain (its cost is not measured).

Not done: no build and no test run. Fast rounds, cohost and admission need
debug-level runs (R1 to R5).

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

### M2b. Drop a seen frame before the heavy work (new, from the profile)

- Idea: a duplicate frame pays for a signature verify (11% to 15% of the busy time),
  and a duplicate `meta` change pays for a JSON parse and an automerge apply (38%
  inclusive). Check the dedup key before the verify, and check that a state change
  is already applied (by its change hash) before the parse and the apply.
- Why it is safe: a frame that was already accepted needs no second verify, and a
  change that is already applied is a no-op for a CRDT. No wire change.
- Open: the order of the checks today. [G] The log lines `state dir="in"` that repeat
  30 times per frame suggest that duplicates reach the apply path.
- Experiment: read the order in `gossip/recv.rs` (3,929 lines). Then a counter of
  verifies and applies per duplicate, and a paired A/B on R4.
- Result: pending. Owner: `origin-magnet`, after M1.

### M2c. Do not serialize the whole meta document for every meta frame (found)

- Fact [V by reading, two sources agree]: the profile's 17.4% (2,713 samples,
  `wire-straw`, call tree) is a full `to_json` of the meta document, reached as
  `gossip rx arm` > `recv::ingest` > `dispatch_infra` > `dispatch_channel` >
  `a2a_app on_meta_applied`. In the engine, `dispatch_channel`
  (`gossip/recv.rs:1141-1163`) calls `app.on_meta_applied(...)` for EVERY meta
  frame that reaches dispatch, although the comment says "post-apply hook" and the
  `change_count() > changes` test just above is used only for the fast rounds. In
  the app, `adopt_meta_endpoint` (`a2a/node.rs:1224`) runs
  `state.doc(Channel::Meta).to_json()`, the whole document (about 100 KB for 45
  peers), only to read one author's card endpoint.
- Cross-check [G]: about 1.6 meta frames per second reach dispatch on my daemon
  (7,883 in 83 min), and 17.4% of 61% of a core is about 106 ms of CPU per second,
  so about 65 ms per call. That is a plausible time to hydrate a 100 KB
  Automerge document.
- Another 18.0% of the samples (2,804) are unattributed Automerge leaf frames under
  `thread_start` (bad unwinding). [G] Probably the same path.
- Fix, two parts, no wire change and no result change:
  1. Call the hook only when the frame changed the document
     (`change_count() > changes`): about 3 new meta changes per minute instead of
     about 1.6 calls per second.
  2. Read only the author's card entry, not the whole document, in
     `adopt_meta_endpoint` (needs a single-path reader in the engine document API;
     check that one exists).
- Risk: a duplicate meta frame today re-adopts the author's endpoint into the dial
  book, which can repair an entry that was evicted in between. That is an
  accidental repair path; a measured gain must pay for losing it. Experiment: count
  hook calls and `to_json` calls per 10 s before and after, a paired A/B on idle
  CPU at 12 members, and the late-join and `loopback_room_split` tests.
- Expected gain: up to 17.4% of the busy time, and up to 35% if the unattributed 18%
  is the same path.
- Result: pending. Owner: `origin-magnet` (engine part) and the leader (app part).

### M2d. The dedup memory is too short for the flood (found)

- Fact [V by reading, `origin-magnet`]: `seen` is a count-bounded FIFO of 2 x
  `MESSAGE_LOG_SIZE` = 2,000 keys (`tuning.rs:17`, `:25`; `event_loop_state.rs:416`),
  not a time window, and it takes the keys of every frame kind. The 90 s window is
  the `iroh-gossip` payload dedup, not ours. Frames not in the message log (alive,
  PeerInfo, digests, linkstate, State and Meta, stale presence) are stopped only by
  `seen`.
- Consequence [G]: at about 800 frames per second the 2,000 keys turn over in about
  2.4 s, which is shorter than the 10 s digest period. So the flood evicts the
  memory that should stop it, and a repeat passes the dedup again. This fits the
  30 repeats per frame in the logs, and it makes the loop self-sustaining.
- Idea: a larger cap (2,000 keys of 16 bytes is only 32 KB, so 64,000 keys is about
  1 MB), or a time-based memory of 60 s or more. No wire change. A repeated hit
  should not push the key again.
- Experiment: a counter of frames per second and of `mark_seen` hits and misses
  (add to the census), then A/B with the cap at 2,000 and at 64,000 on R4.
- Result: pending. Owner: `origin-magnet`.

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
