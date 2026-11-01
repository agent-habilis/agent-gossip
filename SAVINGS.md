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
| none (measurement) | `167b3273` (`habilis-network` `…-counters`, 13 commits on `f7cd9bbc`) | Counters and gauges, no behaviour change | Idle CPU of 12 daemons, 150 s window, 3 valid interleaved pairs | main: 22.25 s, 14.23 s, 14.50 s | head: 19.12 s, 12.92 s, 11.77 s (14%, 9%, 19% below main) | `gold-velvet`: `mesh_peer_load`, `scripts/perf/run.ts`, load 3 to 12. The spread is load noise: the claim is no measurable overhead, not a gain. 1 pair dropped (load 36 to 68). | done; full engine test suite after the last commits still to run |
| G2, G3 | pending | Baseline B4 at 64 daemons | Metadata bytes per delivered data byte; duplicates as a share of received frames; CPU median and worst | pending | not applicable | Runner of `stale-bundle`, 3 pairs | pending: needs part A (counters) and part B+C (runner) |

## Cumulative result

Pending. Baseline: `main` at `f7cd9bbc`. Head: not set. Metrics: idle CPU median
and worst per daemon at 64 daemons; metadata bytes per delivered data byte;
duplicates as a share of received frames; the slope of every memory gauge.
