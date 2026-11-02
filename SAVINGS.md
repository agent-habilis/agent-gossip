# Savings per commit

One row for each commit that aims at a gain. The row is written when the commit
is made. A row with no numbers says `pending` and names the measurement that
fills it. A commit without a row does not go in.

Goals: G1 idle CPU below 1% per daemon in a mesh of 64. G2 less metadata on the
wire. G3 fewer duplicate bytes. G4 less unnecessary work. G5 bounded memory for
a mesh that runs 1 year or more.

How to measure:

- CPU and bytes: paired runs with the runner in `habilis-network`
  `scripts/perf`, at least 3 interleaved pairs, on `nyc-macmini1`. Write the
  median and the spread, the load of the host, the sha and the dirty flag of
  both binaries. Load gate: set from the load of an idle 64-daemon mesh.
- Memory: a size gauge of the collection in the census line, sampled every 30 s
  for at least 2 h. A capped gauge passes if its maximum stays within the cap.
  An uncapped gauge passes if the slope times 365 days stays within a stated
  budget. RSS is reported only.
- A bug fix carries a test that fails first, for the reason of the bug.

Rule: after each commit, the owner of the commit writes or updates its row in
the same work step. The leader checks the ledger before a side branch is merged
into `feat-reduce-resource-consumption`.

Cumulative delta: at the end, measure the head of the branch against the
baseline binary (`main` at `f7cd9bbc`, built once, kept with its sha in
`~/bench-results/baseline/`), with the same runner, the same workload and the
same method as above. The result goes into the section "Cumulative result" at
the bottom. The sum of the rows is not the delta: gains overlap, so only this
run counts.

| Goal | Commit | Change | Metric | Before | After | Method | Status |
|---|---|---|---|---|---|---|---|
| G5 | `25864570` (`…-r17-no-event-capture`) | A directory session holds no event channel | Events held in memory by a directory session | grows without bound (1 event per mesh event, kept for the life of the process) | 0 (no channel) | Test `api::directory_session_holds_no_event_stream`: red on `27f86c9c`, green after. Bytes not measured. | pending: the soak gauge of unbounded channels gives the bytes |
| none (measurement) | `188a27a5` (`habilis-network` `…-counters`, 22 commits on `f7cd9bbc`) | Counters and gauges, no behaviour change; census made O(1) in A2 | Idle CPU of 12 daemons, 150 s window, 3 interleaved rounds of 3 arms, 9 valid runs | main `f7cd9bbc`: 26.33 s mean (17.6% of one core). Counters `167b3273`: 30.12 s (+14.4%) | Counters `188a27a5`: 28.09 s (+6.7% mean, +1.1% to +12.4% per round) | `gold-velvet`: `mesh_peer_load`, `scripts/perf/run.ts`, load 3 to 8 in the windows. Spread of one arm is 3 to 7 s, so the 1.8 s difference is inside the noise. The new arm is above main in all 3 rounds: cost not proven zero. The earlier A1 row (below main by 9% to 19%) was noise. Run with 64 doc frames not done: the runner has no option for it. | done; overhead of the counters is about +7%, to confirm at 64 daemons in B4 |
| G2, G3 | baseline (no commit) | Baseline B4 at 16 daemons on `nyc-macmini1` (8 GiB, load 1.3 to 2.7 idle) | Idle CPU per daemon; metadata bytes per delivered data byte; duplicates share; RSS per daemon | CPU (plain baseline `b588fc75`/`f7cd9bbc`, 3 runs): idle 2.0% [1.7..2.1] of one core. With counters (`188a27a5`): 2.1% [1.3..2.3]. Metadata 7268 B/s per daemon idle [5502..7325]. Ratio metadata per data byte: 78.6 frame layer, 132.6 UDP layer (pairs 2 and 3: 78.6 to 79.2). RSS 52 to 57 MB per daemon | not applicable (baseline) | `stale-bundle`: `scripts/perf` `b4.ts`, 3 interleaved pairs, gates start 2.0/2.5, phase median 4.0 max 6.0, A/B 1.35. Pair 1 failed A/B twice (1.74, 1.86): weak, use pairs 2 and 3 for the ratio. Workload small (577 kB data in 300 s): the ratio is dominated by idle metadata. Duplicates (counters arm, pairs 2 and 3): 28.4% to 29.6% of received frame bytes, 18.2% to 18.5% of received frames; 97% of the received meta bytes are duplicates. Received bytes by kind: digest 42% to 44%, meta 28% to 29%, metadigest 15% to 16%, statedigest 6%, app 2.7%. Wire in 211 MB for 577 kB of data. Goal for the ratio test: half of the baseline, so about 39 at the frame layer at N=16. CPU goal 1% per daemon: baseline is 2.0% at N=16, so it is 2 times too high already at N=16. | done at N=16 and for the CPU curve; 64 daemons is an extrapolation. CPU curve of the plain baseline, `nyc-macmini1`, 3 runs per N, idle windows, percent of one core per daemon: N=8 0.8 [0.7..0.9]; N=12 1.4 [1.4..1.5]; N=16 1.9 [1.3..2.2]; N=20 1.7 [1.6..2.4]. Mean RSS per daemon 33.4, 42.4, 56.1, 74.9 MB. Above 1% from N=12. The spread at N=16 and N=20 is as large as the step (host load 1.7 to 2.2). Line through N=8 and N=16 gives about 8.4% at N=64: an extrapolation (🟡 70%, the N=20 point says it may flatten), not a measurement. |
| G5 (G4 not claimed) | `db8a2a11` (`…-state-changed`) | The surfaced ring held one deep copy of the document per `StateChanged` event; a Meta change now holds no document in the ring (its event body, a patch, stays: bound is 1000 times one patch, no byte cap on a patch), and the live copy and the ring copy of a State change share one `Arc` | N=16 real daemon, node 1: live heap under `Output::state_changed`; total live heap; RSS mean | Live heap under `state_changed` 1.54 MB (12.0k allocations); node 1 total live heap 17.8 and 18.3 MB (about 33k allocations); RSS mean 54.1 MB | 0 MB (0 allocations); total live heap 15.0 and 15.7 MB (about 20.8k allocations); RSS mean 49.9 MB (-4.2 MB, -7.8%, same size as the noise) | `gold-velvet`: `MallocStackLogging` (allocation bytes do not depend on load), 2 rounds per arm, plus 4 plain runs (before, after, after, before), load 3 to 8. Binaries: `sha256` `4174e1be` (before, `5ab05749`) and `9257d638` (after, `db8a2a11`); the version text of both says `5ab05749`, so use the record JSON. Test `the_ring_copy_of_a_meta_change_holds_no_document`: red first (40000 bytes), green after. CPU: 2.75 s to 2.33 s, spread as large as the step, no claim. Remainder: the ring still holds State-channel documents (count times document size), see R12. | done and merged (head `da4f365a`; `cherry-angle` reviewed 3 times, last answer: clean to merge) |

## Cumulative result

Pending. Baseline: `main` at `f7cd9bbc`. Head: not set. Metrics: idle CPU median
and worst per daemon at 64 daemons; metadata bytes per delivered data byte;
duplicates as a share of received frames; the slope of every memory gauge.
