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
