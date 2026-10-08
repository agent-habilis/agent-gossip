# Changes requested in habilis-network

agent-gossip found these while it moved from fofoca to habilis-network
(branch `chore-migrate-habilis-network`). They were seen against
habilis-network PR #2 at `2b468bb`. Nothing here is applied: the squad that owns
habilis-network decides.

1. **Re-export the transport ids.** Export `WEBRTC_TRANSPORT_ID`,
   `MULTIHOP_TRANSPORT_ID` and `GOSSIP_TRANSPORT_ID` from `habilis_network::net`.
   agent-gossip copies them by hand in `a2a/peer_path.rs` to name the path of a
   peer. A copied value drifts silently if the engine changes it.
2. **Make `LookupSet::any` public.** agent-gossip reimplements it in
   `api/directory.rs` (`resolve_lookups_or_public`).
3. **`FORKED.md` is stale.** Lines 23-40 list crates that no longer exist
   (`-blobs`, `-reassembly`, `-directory`). Lines 365-414 tell consumers to
   carry a `[patch.crates-io]` table. The root `Cargo.toml` says that no patch
   table exists anywhere.
4. **The `TransportOpts.gossip` comment is stale.** The comment at
   `lookup/mod.rs:168-171` says gossip is "off by default". `within()` now sets
   it from the policy, and the default policy has gossip on (b49b3d2).
5. **A fofoca name is left in `Cargo.toml`.** The comment at root
   `Cargo.toml:119` still names `fofoca-network/iroh#2`.
6. **A CHANGELOG line about agent-gossip is wrong.** The line "agent-gossip:
   multihop is now ON by default" is not true after this migration.
   agent-gossip sets its own policy, `udp,webrtc,gossip` plus `relay` when the
   relay lookup is on, so multihop is off unless the user asks for it.
7. **Directory meshes ignore the app's transport policy.**
   `habilis_network_protocol::directory_config` (`protocol/directory.rs:69-76`)
   hard-codes `TransportPolicy::default()`, which is `udp,webrtc,multihop,gossip`.
   So every directory session of agent-gossip (`--advertise`, `discover`,
   `a2a expose`, `a2a discover`) binds the multihop underlay, but agent-gossip
   wants multihop off. Let `directory_mesh` take a `TransportPolicy`, or make the
   directory default direct-only.
8. **A chat broadcast sent before the creator's first real-peer link is not
   recovered.** Repro: three in-process nodes, `create` plus two `join`s; both
   joiners broadcast right after their join returns (2 to 15 ms later); the
   creator waits for the messages (`tests/monitor_contract.rs`
   `test_cross_peer_message_delivery` and `test_bidirectional_multi_peer`).
   The creator links to its first real peer about 0.4 s later and logs "asked
   for state and chat again on the first real-peer link" (`gossip/recv.rs:131`).
   One message then arrives (0.5 s), the other never does, not in 60 s, although
   the State and Meta digests keep running. Numbers, 30 tests in parallel, macOS,
   `ci` profile, whole binary, 10 runs each: engine 2b468bb with gossip transport
   on 5 fail; engine 2b468bb with gossip transport off (`udp,webrtc`) 5 fail;
   previous engine (main 37e2135) 0 fail. So the cause is not the gossip
   transport. The chat digest writes no log line, so a run cannot show whether
   it was sent, answered or ignored (the Meta digest logs "ignored: this asker
   was served within the window"). Suggested: add a debug line to
   `antientropy::broadcast_digest` and to the chat answer path (sent, answered
   with n frames, ignored by the asker window). CI on Linux flakes the same two
   tests (PR 38, run 37815591169). A `three_peers` that waits for the cards would
   probably make the suite pass and would hide this problem, so the app keeps the
   test as it is. Fix candidate 5e8727c ("a digest asks for the whole second of
   the first message it holds"): the two tests passed 10 of 10 runs against it
   (the whole binary passed 9 of 10, see item 9).
9. **A presence message sent at a late first link is not recovered.** Found in
   the verification run of fix candidate 5e8727c (10 runs of `monitor_contract`,
   macOS, `ci` profile): 1 of 10 runs failed `test_peer_discovery_three_peers`
   ("expected >=2 joined presence events", `monitor_contract.rs:283`). The
   creator received the presence of joiner `mon-disco-a` (58.681) and never the
   presence of joiner `mon-disco-b`, which `mon-disco-b` logged as sent at 59.922,
   6.7 s after it started (30 tests run in parallel on the host). The State and
   Meta digests kept running every 10 s and did not bring it in 60 s. It is the
   same class as item 8, on the presence channel. The log does not show whether
   `mon-disco-b` was linked to the creator or only to `mon-disco-a`, so the
   fix of 5e8727c may or may not cover it. A second series of 10 runs on the
   same build (copy of HEAD 1a14c16, engine 5e8727c, iroh-gossip a9fcab9) passed
   10 of 10, so the combined rate against 5e8727c is 1 failure in 20 runs, and
   it was the presence test only. The previous 20 runs of engine 2b468bb
   (item 8) failed only the two chat tests. Log, colors stripped:
   `/private/tmp/claude-501/-Users-caiogondim-Developer-agent-habilis-agent-gossip-chore-migrate-habilis-network/e634f801-6033-4c50-84a8-60f402a02efd/scratchpad/fix-run-4.plain`
   (a passing run: `fix-run-1.log` in the same folder).
