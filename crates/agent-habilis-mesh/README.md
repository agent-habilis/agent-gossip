# agent-habilis-mesh

The serverless gossip-network **engine** — everything that moves, signs,
routes, gates, and heals bytes between peers, with no knowledge of what those
bytes mean. It is the crate [`agent-gossip`](../agent-gossip) is built on, and
it is deliberately not the crate that knows about A2A — `cargo task layering`
fails the build if it learns.

Peers find each other without a server, form a partial mesh over
[iroh](https://github.com/n0-computer/iroh) QUIC links, and keep the mesh alive
across creator departure, sleep, network switches, and churn. Every frame is
Ed25519-signed and verified on receipt; directed frames may additionally be
sealed to their addressee.

## Why it is a separate crate

The engine/application split is a boundary, not a folder. `agent-gossip` owns
the A2A data model, the CLI, the MCP server, and the library `api`; this crate
owns the transport and protocol beneath all three. The split buys three things:

- **The payload stays opaque.** The engine routes on a frame's tag and
  addressee and never parses its body. That claim is *enforced* by having a
  second, independent consumer: [`examples/mesh-pipe`](../../examples/mesh-pipe)
  pipes raw bytes over the same mesh, depends on this crate only, and never on
  `agent-gossip`. Let an application assumption leak down into the engine and
  mesh-pipe stops compiling — and `cargo task layering` catches the naming half
  of the same mistake before that.
- **iroh stays an internal detail.** No `iroh` type crosses `agent-gossip`'s
  public surface. Version bumps and the forked revs pinned in the workspace
  root's `[patch.crates-io]` are contained here.
- **The wire is testable on its own.** The frame format, every crypto
  byte-domain, and `runtime_base()` live here, so `cargo task test` runs
  `--workspace` rather than `-p agent-gossip` — scoped to the app, a stale
  engine snapshot stayed green.

It is workspace-internal (`publish = false`). `agent-gossip` re-exports the
curated public surface, so downstream code names `agent_gossip::MeshId`, not
`agent_habilis_mesh::protocol::MeshId`.

## The application seam

Two traits, both `#[async_trait]`, are the whole contract an application
implements. The engine owns the event loop and the mesh state; it hands the app
each frame *after* parse → signature verify → mesh gate → dedup → shard
reassembly → unseal.

- **`gossip::app::NodeApp`** — inbound. `classify(&Message) -> AppClass` states
  five per-frame wire policies (`loggable`, `beat`, `valid`, `chained`,
  `sealed`) so the engine can decide retention, surfacing, and DAG indexing
  without knowing the app's tags. `on_app_frame` dispatches. Every other hook
  (`surface_logical`, `on_meta_applied`, `on_meshed`, `on_peer_left`) has a
  default no-op body.
- **`daemon::app::NodeDriver`** — a superset adding the app's timers, lifecycle
  hooks (`on_startup` / `on_tick` / graceful shutdown), and three associated
  types for its own inputs (`Session`, `Http`, `Ipc`), all opaque to the engine.

`daemon::Node<A: NodeDriver>` is the app-agnostic handle that runs the loop
in-process. `agent-gossip`'s `api::MeshSession` wraps it rather than
duplicating it.

> A minimal receive-only consumer implements `classify` + `on_app_frame`, sets
> the three associated types to trivial types, and takes the defaults for
> everything else — about 40 lines. mesh-pipe is exactly that.

Outbound, the payload-agnostic primitive is
`gossip::send_app(state, ctx, tag, to, corr, body)` — build → sign → route.

## Subsystem map

Each module owns one layer's vocabulary; the workspace glossary rule is that a
layer never borrows another layer's term.

| Module | What it owns |
|---|---|
| `protocol` | The wire: the `Message` envelope and its value types, `crypto` byte-domains, seed-derived `identity`, `mesh` ids/hashing, `nickname`, `peer_addr`, `seal` |
| `daemon` | The shared event loop behind `create` and `join` — loop `state`, `ctx`, timers, the message log, the session `state_file`, and the generic `Node` handle |
| `gossip` | The message plane: `broadcast` (send, presence, buffering) and `recv` (the event pump, neighbour bookkeeping, per-message handling) |
| `transport` | Everything that moves bytes on either plane — the cross-plane `deliver` decision, the `Lane` it picks, the unicast `sender`, and the `ipc` socket server |
| `lifecycle` | A peer's presence over time: heartbeat, membership transitions, and the join-horizon `joined`/`left`/`peer_return`/`peer_timeout` decisions |
| `beacon` | The beacon role: co-hosting the rendezvous endpoint so bootstrap outlives the creator |
| `lookup` | Building the iroh endpoint per mesh mode and wiring the selected mechanisms: `mdns` (LAN), `dht` (mainline), `relay` (the ladder + failover) |
| `resolver` | Classifies what `join` accepts, once, at the boundary: `JoinTarget::Mesh` (a bare base58 mesh id, no I/O) or `JoinTarget::Invite` (a bare base58 invite ticket) |
| `invite` | Minting and redeeming bare base58 bearer tickets to an invite-only mesh — signed, TTL'd, carrying the derivation root a bare hash withholds |
| `doc` | The CRDT engine ([`automerge`](https://automerge.org)) behind the `state` and `meta` channels; a local write is an RFC 7386 JSON merge |
| `blob` | Direct point-to-point transfer of artifacts too large to inline, content-addressed by SHA-256 over a dedicated QUIC endpoint, off the gossip plane |
| `reassembly` | Splitting and rebuilding bodies past `MAX_MESSAGE_SIZE`, with repair tickets and byte budgets on every buffer so a crafted shard cannot exhaust memory |
| `directory` | Opt-in mesh discovery — "meshes all the way down": a mesh created with `--advertise` re-broadcasts itself into a directory mesh |
| `logging` | The `tracing` directive filter, the deferred per-member file sink, and the per-message logger |
| `util` | Cross-cutting helpers, the build version stamp, and `consts` — where every tunable knob lives |

It also re-exports [`iroh_multihop_transport`](../iroh-multihop-transport) so
the app can name types like `LinkVector` without a second direct dependency,
and re-exports `async_trait` so a consumer annotates its seam impls as
`#[agent_habilis_mesh::async_trait]` rather than version-matching its own.

## Test it

There is no `tests/` directory here: coverage is unit tests and `proptest!`
blocks inside `src/`, plus snapshots that pin the frame format. The behavioural
and wire-contract suites live in the app crate, because they exercise the
engine through the surface users actually get.

```sh
cargo test -p agent-habilis-mesh     # this crate's unit + property tests
cargo task test                      # the whole workspace suite
```

> Run the full suite in the background — it takes minutes. The remaining floors
> are iroh-bound (a 15s direct-path idle timeout, a ~36s beacon handoff), not
> ours.

Four features, all off by default and never in a release build:
`test-fixtures` (exposes cross-crate `Message` builders), `bench` and
`adversarial` (both imply it, exposing `pub(crate)` hot paths and
crafted-message constructors to the app's harness), and `dhat-heap` (gates out
the daemon's `process::exit` so a heap profile can flush).

`build.rs` stamps the git version via
[`vergen-gitcl`](https://crates.io/crates/vergen-gitcl) into
`util::version::VERSION`.

## Two things that will bite you

**Write an explicit `target:` on every `tracing` call.** Use
`target: "agent_habilis_mesh::<subsystem>"` — one of `lookup`, `gossip`,
`lifecycle`, `beacon`, `directory`, `messages`. The targets match this crate's
own path on purpose, so a call in the module that owns its subsystem is also
covered by the default (module-path) target; only cross-module calls depend on
the explicit one — `reassembly` logs to `gossip`, and `messages` lives at
`logging::messages`.

Write it anyway, because the pins in `logging::log_filter` are what keep
connectivity logs at `info` in a release build, whose base level is `error`. A
line whose target matches no pin compiles, passes review, works in a debug
build, and is silently dropped from every optimized one. This is not
hypothetical: when the beacon and gossip-neighbour lines lost their targets, a
test asserting on them failed 100% under `--profile ci` while passing locally,
and shipped release binaries had no beacon diagnostics at all.

**There is no environment-variable config.** Every knob is a `const` in
`util::consts` — edit and commit to experiment. Only `RUST_LOG` and `NO_COLOR`
are read from the environment. The few values the test suite must vary per-run
are hidden CLI flags on the app binary (`#[arg(hide = true)]`), not env vars.

## What it is not

Not a general-purpose networking library, and not published. It has exactly the
generality its two consumers proved it needs — an agent application and a byte
pipe — and no speculative extension points beyond the two seam traits. Bulk
transfer is not its job either: gossip re-broadcasts and logs every frame, so
large payloads go over the `blob` channel or not at all.
