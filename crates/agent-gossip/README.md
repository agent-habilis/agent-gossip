# agent-gossip

The application crate — the A2A data model, the three frontends that expose it
(CLI, MCP server, Rust library), and the shipped `agent-gossip` binary. The
mesh underneath it is [`fofoca`](../fofoca).

**This file is for people working on the crate.** For what agent-gossip *is*,
how to install it, and how to use the skills, read the
[root README](../../README.md). For the user-facing command reference, run
`agent-gossip man` (source: [`docs/manual.txt`](../../docs/manual.txt)).

## Why the app and the engine are separate

Everything below the A2A layer — frames, signatures, routing, gating, healing,
the CRDT documents — lives in the engine,
[**fofoca**](https://github.com/fofoca-network/fofoca), which is a separate
repository with three consumers; this crate is one of them, alongside
`agent-share` and `mallorca` (the latter through fofoca's C ABI). The split
keeps the payload opaque to the transport — an application assumption that
leaked downward would break the other two — and keeps `iroh` off this crate's
public surface entirely: a join target is a mesh id parsed from a string, so
iroh version bumps never reach a library consumer.

This crate re-exports the curated engine surface — `MeshId`, `Message`,
`MessageBody`, `Nickname`, `InviteTicket`, `RosterSnapshot`, `Lane`,
`JoinTarget`, the tunables in `util::consts` — so downstream code names
`agent_gossip::MeshId` and never depends on the engine directly.

## Three frontends, one event loop

The same `NodeDriver` implementation backs all three. Nothing is reimplemented
per binding.

| Frontend | Module | Shape |
|---|---|---|
| CLI | `cli` (`pub(crate)`) | 20 clap subcommands. `create`/`join`/`topic` run the event loop as a daemon; the commands that operate on a live gossip reach it over that daemon's Unix socket |
| MCP | `mcp` (`pub(crate)`) | `agent-gossip mcp` serves the same operations over MCP stdio, plus the embedded manual |
| Library | [`api`](src/api/mod.rs) | `api::MeshSession` runs the loop as a background `tokio` task **in the caller's process** — no subprocess, no socket |

```rust
use agent_gossip::api::{JoinConfig, MeshSession};
use agent_gossip::MessageBody;

let session = MeshSession::join(JoinConfig::new("<hash>".parse()?)).await?;
let mut rx = session.messages();
session.send(MessageBody::new("hello")?).await?;
while let Ok(msg) = rx.recv().await {
    println!("{} : {}", msg.author, msg.body);
}
session.leave().await?;
```

Inbound traffic arrives on a bounded broadcast channel; outbound sends go
through a dedicated channel into the same shared broadcast path the CLI's IPC
uses. Beside `MeshSession` the API exposes `JoinConfig` / `CreateConfig` /
`TopicConfig`, `Directory` (mesh discovery), and the `JoinError` / `CreateError`
pair.

## Module map

Public: `a2a`, `api`, `events`. Everything else is `pub(crate)`.

- **`a2a`** — the agent-communication data model, public on purpose because it
  is what both *bindings* carry: the always-on gossip binding and the
  flag-gated localhost JSON-RPC binding. (Bindings are how A2A travels;
  frontends are how a caller drives it. Different axes.) `model` holds the
  A2A v1.0 ProtoJSON types
  (`AgentCard`, `Task`, `TaskState`, `Message`, `Part`, `Role`, `Artifact`,
  `PROTOCOL_VERSION`); `extensions` holds the `mesh/*` extension URIs
  (broadcast, seal, blob, state, rpc). Inside it, `node` implements the engine's
  `NodeApp`/`NodeDriver` seam, `task` the task state machine, `send` the
  outbound paths, `surfaced` the surfacing rules, `gossip_rpc`/`rpc` the
  request/response plane, and `ipc` the Unix-socket command set.
- **`bridge`** — the A2A HTTP tunnel behind `a2a expose` / `a2a connect`:
  tickets, Agent Card URL rewriting so discovery resolves through the
  bridge, and its own ALPN. A bridge subsystem, not A2A core.
- **`output`** — the stdout event stream. `events::OutputEvent` is the typed
  event; `output::json` serializes it.
- **`harness`** — `#[doc(hidden)]`, feature-gated. The testkit and the
  crafted-message injector live here rather than in `api` so a test feature
  cannot widen the public surface (nor put an `iroh` type on it).

### Command groups

`create` / `join` / `topic` start a daemon; `leave` stops it; `session` is
`leave`'s read-only sibling (how an agent that lost its conversation context
re-learns its gossip id and nickname). `poll` reads messages, `ping` measures
RTT, `peers` lists the roster, `topology` and `ready` report mesh shape and
readiness. `state` and `meta` read and merge the shared CRDT documents.
`invite` mints tickets; `discover` browses advertised gossips. `a2a` is
the A2A surface — `call` (a `SendMessage` with `--to` is a directed
request/response, without `--to` it broadcasts), `status` and `artifact`
(worker-emitted task updates), `fetch` (blob artifacts), and
`expose`/`connect`/`discover` (the HTTP bridge). `mcp` serves MCP stdio, `man`
prints the manual, `plug`/`unplug` install and remove the agent integrations,
and `doctor` diagnoses the machine and the network.

> **Output invariants.** `plug`, `unplug`, `man`, and `doctor` print for a
> human; every other command is JSON-only, and a live skill parses each. JSON is
> never colored — color lives only in `util::output` and `doctor`'s
> `render_human`, both written through [`anstream`](https://crates.io/crates/anstream),
> which resolves color support per stream at write time. So there is no
> `--color` flag, no `is_terminal()` call, and no env read of our own. And
> stdout is the product: the roster *is* `plug`'s output, so `status` prints to
> stdout — only real errors go to stderr.

## Reaching up out of the crate

`docs/` and `skills/` stay at the repo root, so this crate reaches up for them.
Both paths are load-bearing:

- **`build.rs`** renders the multi-file `../../skills` sources through
  [`slot-template`](../slot-template) into the single-file `SKILL.md` tree the
  binary embeds with `include_dir!`, then emits a fingerprint of the *generated*
  output — `include_dir!` is otherwise untracked on stable, and fingerprinting
  the sources would miss renderer-only changes. Adding a skill needs no change
  to `build.rs`.
- **The manual** is an `include_str!` of `../../../../docs/manual.txt` from
  `src/{cli,mcp}/mod.rs`. Edit that file to change `agent-gossip man`.

The git version stamp lives in the *engine's* `build.rs`, since `util::version`
is an engine module.

## Test it

```sh
cargo task test     # the full suite (run it in the background — minutes)
cargo task lint     # clippy over the whole workspace, warnings denied
cargo task ci       # the CI gate: fmt --check, then lint, then test
cargo task run create   # cargo run -- create
```

`tests/` holds 18 integration binaries in three layers:

- **In-process (default, fast).** Behavioural and output-schema tests drive the
  real event loop through the library `api` via
  `agent_gossip_test_fixtures::InProcNode`. Real iroh mesh, no subprocess —
  sub-second.
- **Every-run subprocess.** The wire-contract suite (CLI / stdout /
  `--output json` / Unix socket / MCP stdio) plus reliability invariants that
  need real OS processes and signals — SIGKILL beacon migration, SIGSTOP/CONT
  heal recovery, anti-entropy backfill. Mostly `tests/gossip_network.rs`.
- **Adversarial** (`--features adversarial`, `tests/adversarial.rs`). An
  in-process attacker injects crafted wire bytes a correct client never
  produces. Defended cases pass; open-gap `#[should_panic]` tripwires go red the
  moment a gap is closed.

All three share one harness:
[`agent-gossip-test-fixtures`](../agent-gossip-test-fixtures), a dev-dependency
that depends back on this crate (cargo permits the cycle because the back-edge
is a dev-dependency).

Three features, all off by default and never in a release build: `bench`
(exposes the engine's hot paths to `benches/hot_paths.rs`), `adversarial`, and
`dhat-heap` (installs dhat's allocator and makes the CLI quit path return
cleanly so the profiler flushes).

> **No environment-variable config.** Every knob is a `const` in the engine's
> `util::consts` — edit and commit to experiment. The few the suite must vary
> per-run are hidden CLI flags (`#[arg(hide = true)]`: `--alive-timeout-secs`,
> `--heal-interval-secs`, `--log-dir`, `--log-raw`). Only `RUST_LOG` and
> `NO_COLOR` are read from the environment.

Daemon logs land in `<log_dir>/<mesh_prefix>-<nick>.log` (default: the
`agent-gossip/logs` subdir of the OS temp dir; `cargo task logs` prints the
path). **Message bodies are redacted by default** so a log is safe to share.
The `--output json` stdout stream is a separate path and is always raw.
