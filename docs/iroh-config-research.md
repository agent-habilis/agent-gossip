# Core-lib config research: iroh & iroh-gossip

Second survey of the [awesome-iroh](https://github.com/n0-computer/awesome-iroh)
subset, focused on how projects configure the **core libraries** — the iroh
`Endpoint` (relay, transport/QUIC) and iroh-gossip (`proto::Config`: HyParView
membership + Plumtree broadcast + `max_message_size`). Companion to
[iroh-ecosystem-research.md](./iroh-ecosystem-research.md).

Headline: **we ran almost entirely on library defaults** —
`Gossip::builder().spawn(endpoint)` with no config and an `Endpoint` builder
with no `transport_config`. p2panda-net is the only surveyed project that tunes
these deliberately; dtt, iroh-gossip-discovery, and jamessizeland take the
defaults. The survey surfaced two latent bugs and one mismatch, now addressed
(see "Changes made").

## iroh-gossip `proto::Config` (the tunable surface + defaults)

Set via `Gossip::builder().max_message_size(..)` / `.membership_config(..)` /
`.broadcast_config(..)`.

| Param | Default | Notes |
|---|---|---|
| `max_message_size` | **4096 B** | usable payload ≈4057 B after the ~39 B wire header; a message over this **silently fails to propagate** (p2panda #628) |
| membership `active_view_capacity` | **5** | active gossip neighbors (HyParView paper p9) |
| membership `passive_view_capacity` | 30 | remembered-but-not-connected peers |
| membership `shuffle_interval` | 60 s | passive-view shuffle cadence |
| membership `neighbor_request_timeout` | 500 ms | |
| broadcast `graft_timeout_1` / `_2` | 80 ms / 40 ms | Plumtree repair (request a missed payload) |
| broadcast `dispatch_timeout` | 5 ms | lazy `IHave` push delay |
| broadcast `message_cache_retention` | 30 s | how long a broadcast is replayable to peers |
| broadcast `message_id_retention` | 90 s | dup-suppression window |

## iroh `Endpoint` builder (the transport/relay surface)

| Param | iroh default | Notes |
|---|---|---|
| `relay_mode` | N0 multi-relay | `RelayMode::{Default, Custom([..]), Disabled}` |
| `transport_config` (`QuicTransportConfig`) | path idle **15 s** direct / **30 s** relay; ~1 s keep-alive | governs how fast a dead path / peer is detected |
| `address_lookup` | none on `Minimal` | mDNS / pkarr-DHT builders |
| `portmapper_config` | on | UPnP/PCP/NAT-PMP to the gateway |

## Who tunes what

- **p2panda-net** — the reference. Exposes a full `GossipConfig`
  (`p2panda-net/src/gossip/config.rs`, re-exporting `HyParViewConfig` /
  `PlumTreeConfig`) but keeps the gossip defaults (incl. `max_message_size`
  4096, `active_view_capacity` 5). It *does* tune the endpoint
  (`iroh_endpoint/actors/endpoint.rs`): `keep_alive_interval = 5 s`,
  `max_idle_timeout = 10 s`, and `RelayMode::Custom(relay_map)`.
- **distributed-topic-tracker** — `Endpoint::builder(presets::N0)` + gossip
  defaults; its rich config system is for *its own* bootstrap/DHT layer, not the
  iroh/gossip libs.
- **iroh-gossip-discovery / jamessizeland** — gossip + endpoint defaults.
- **us (before this work)** — all defaults: no gossip config, no
  `transport_config`.

## Gaps found (and resolved)

1. **Silent message loss — bug.** Our app `MAX_MESSAGE_SIZE` was 16 KB while
   gossip enforces 4096 B and we never set it. A 4–16 KB message passed our
   `serialize()` check, `broadcast()` returned `Ok`, but it exceeded gossip's
   payload budget and never propagated — the sender saw success, the receiver
   got nothing, no error either side. (Exactly p2panda #628, in our code.)
2. **Slow dead-peer detection.** With iroh's default 15 s/30 s path idle and no
   `transport_config`, `NeighborDown` after a peer died/slept was slow (seen in
   the `kill -9` smoke test) — which delays heal and re-bridge.
3. **`--max-peers` (25) vs `active_view_capacity` (5).** The gossip overlay only
   maintains 5 active neighbors, so our 25 ceiling was never reached.

## Changes made

- **Message size (symmetric, no silent loss).** Lowered the cap to
  `ahs_shared::MAX_MESSAGE_SIZE = 3840` (under gossip's ~4057 budget). Both
  `serialize()` (sender) and `parse()` (receiver) enforce it identically, so an
  oversize message is rejected on the sender with a clear "too large" error
  rather than silently dropped. A compile-time tripwire
  (`protocol::message`) asserts `MAX_MESSAGE_SIZE + 256 <=
  iroh_gossip::proto::DEFAULT_MAX_MESSAGE_SIZE`, so an iroh-gossip bump that
  lowers the limit fails the build, not production.
- **Transport timeouts.** `build_endpoint_for_mode` now sets
  `keep_alive_interval = QUIC_KEEP_ALIVE_SECS (5)` and
  `max_idle_timeout = QUIC_MAX_IDLE_SECS (10)` on every endpoint (mirrors
  p2panda) — a dead/slept peer drops in ~10 s while keepalives keep a
  quiet-but-live peer up.
- **Active view.** Left at the default 5 (matches p2panda; plenty for small
  agent swarms). `--max-peers` is documented as a soft ceiling, not the live
  connection count.

All three config values live in `ahs-shared` (a network-wide contract, like
`RATE_LIMIT_PER_MIN`); the crate stays dependency-free, so `MAX_MESSAGE_SIZE` is
hardcoded with the binary-side tripwire guarding it against the live gossip
constant.
