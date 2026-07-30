# AGENTS.md — Instructions for AI Agents

agent-gossip is a serverless gossip network that lets AI agents exchange
messages without a central server. Peers communicate exclusively through the
A2A protocol (**v1.0**, ProtoJSON; gossip frame wire version 12.0) carried over
two bindings — the always-on gossip binding and the flag-gated localhost
JSON-RPC binding — see [`docs/a2a-binding.md`](docs/a2a-binding.md). This file is guidance for working **on**
the project; user/agent-facing usage of the `agent-gossip` CLI lives in `agent-gossip man`
(source: `docs/manual.txt`).

## Concept Glossary

One concept, one word. The codebase is organized in layers; each
layer owns a term and never borrows another layer's. When reading or
changing code, keep these distinct.

See [`docs/glossary.md`](docs/glossary.md) for the full term list, the
layering, and the invariants that follow from it.

## Comments

This codebase keeps comments sparse. Follow these rules:

- **Only explain *why*.** A comment must add what the code cannot say on its
  own — rationale, a trade-off, a non-obvious constraint, or a gotcha that
  would trip up the next reader. If a comment merely restates what the code
  plainly does, delete it; the code is the source of truth for *what*.
- **No file-header comments.** Do not open a file with a `//!` module-doc
  block (or any banner) describing what the file is. The module path and its
  contents already say that.
- **Drop derivable doc comments.** A `///` that just paraphrases a function's
  name and signature is noise — remove it. Keep a doc comment only when it
  carries a *why* the signature can't.

### Load-bearing comments — keep these

Some comments are not commentary; removing them changes behavior or breaks
the build:

- **clap `///` docs** on `Commands` / `Args` / `Task` variants and fields
  render as the CLI's `--help` text.
- **`# Errors` sections on `pub` functions** are required by clippy
  `pedantic` (`missing_errors_doc`). `pub(crate)` and private functions
  don't need them.
- **`#[expect(..., reason = "...")]` / `#[allow(..., reason = "...")]`** — the
  `reason` is mandated by the `allow_attributes` lints, not optional prose.

## Development

All dev tasks run through `cargo task` — run it with no arguments to list
every subcommand.

### Workspace layout

The root `Cargo.toml` is a **virtual manifest** — it owns no package. Every
crate lives under `crates/` (`agent-gossip` the app, `agent-habilis-mesh` the
engine, `iroh-multihop-transport`, `slot-template`, and the dev-only `tasks`),
with `examples/mesh-pipe` as a second engine consumer.

Three things in the root manifest are load-bearing *because* it is virtual, and
dropping any of them changes the build silently:

- **`resolver = "3"`** — a virtual workspace inherits nothing from its members,
  so it otherwise defaults to the edition-2015 `resolver = "1"` and unifies
  dev-/build-dependency features into the shipped binary.
- **`default-members = ["crates/agent-gossip"]`** — an unscoped `cargo build` /
  `test` / `clippy` at a virtual root means *all* members. This pins it to the
  app; widening coverage is a deliberate change, not a default.
- **`[profile.*]` and `[patch.crates-io]`** — cargo honours these only in the
  workspace root. They cannot move into a member manifest.

#### The engine's public surface

`agent-habilis-mesh` exposes **six** modules, grouped by what a consumer needs
rather than by the engine's internal topology. Everything else is `pub(crate)`.

| Module | What it is for |
|---|---|
| `protocol` | Wire value types — `Message`, `MeshId`, `Nickname`, `JoinTarget`, … **Flat**: the submodules are private, so a consumer writes one import path, and the engine can rearrange its files without churning them. |
| `embed` | The seams you implement (`NodeApp`, `NodeDriver`, `NodeSink`) and the values they are handed (`EventLoopState`, `HandlerCtx`, `NodeEvent`). |
| `runtime` | Standing a node up: `*Params::resolve` → `setup_mesh` → `Node::spawn` / `run`, plus `state_file`, `ipc`, `tuning`. |
| `ops` | What a hook may *do*: `deliver`, `broadcast_*`, `doc`, `blob`, `directory`, `invite`. |
| `net` | The quarantined `iroh` corner — endpoint construction and reachability probes. Every other module is iroh-free so a consumer's surface can be. |
| `util` | Host helpers: runtime paths, clock, `logging`, process, version. |

`EventLoopConfig` and `EventLoopState` are **opaque** — accessors only. Both were
once bags of public fields, and three of the config's were patched in after
construction, which meant a window where the value was knowingly wrong.

`docs/`, `skills/`, and `assets/` stay at the repo root, so the app reaches up
for them: `build.rs` renders `../../skills`, and the embedded manual is an
`include_str!("../../../../docs/manual.txt")` from `src/{cli,mcp}/mod.rs`.

### The engine knows nothing about A2A

> The engine may know it carries *an application payload*. It may not know which
> application, what that payload means, or what identity the product stamps on
> the wire.

`cargo task layering` enforces this (and runs first in `cargo task ci`): no file
under `crates/agent-habilis-mesh/src` may contain `a2a`, `agent_gossip::`, or a
`b"agent-gossip…"` byte-domain. It fails with the offending lines and this
rationale. `examples/mesh-pipe` is the live proof — it consumes the engine and
never the app.

Practical consequences when adding to the engine:

- Name the **mechanism**, not the consumer: `http_serve`, not `a2a_serve`;
  `NodeEvent::Ready.http_port`, not `a2a_port`. The app renames at its own
  boundary (`output/mod.rs`) because `a2a_port` is a documented JSON key.
- Push app vocabulary into config. The `meta` channel's per-peer write gate is a
  `doc::SelfWriteGate { map, field }` the app fills in with `peers`/`card`; the
  engine only plants the genesis and compares before/after.
- Push app fields into data. `StateFile::set_discovery` takes an opaque JSON map;
  the app puts `a2a_port`/`a2a_token` in it.
- Ticket kinds pass their own byte-domain to `TicketAuth::derive`.
- Engine tests use neutral tags (`app_msg`, `app_req`) and model-neutral bodies.
  A snapshot pinning a *real* A2A payload belongs in the app crate — see
  `a2a::model`'s `snap_a2a_req_frame_wire`.

Two deliberate exclusions, both load-bearing: the `/tmp/agent-gossip-<uid>`
runtime base (`skills/shared/daemon-session.md` hardcodes that path, so renaming
it orphans running daemons) and user-facing error text naming the CLI. Neither
puts A2A in the engine and neither reaches the wire.

### Testing

`cargo task test` / `cargo task ci` run the suite. **Always run tests in the
background**: most reliability tests inject short cadences via the hidden
tuning flags (`--heal-interval-secs`, `--antientropy-interval-secs`) and poll
observable markers instead of sleeping fixed floors, but the suite still
takes minutes end to end. The remaining floors are iroh-bound, not ours:
the 15s direct-path idle timeout floors the freeze-window tests, the two
beacon-migration tests keep a fixed ~36s handoff wait at the production heal
cadence (see `RENDEZVOUS_HANDOFF` in `crates/agent-gossip/tests/gossip_network.rs` — shortening
the cadence there trips a zombie-link pathology), and the serial-gated
reliability section runs one test at a time.

The suite shares one harness, the **`agent-gossip-test-fixtures`** crate
(`InProcNode`, the subprocess `Node`, the `cli_*` helpers). It is a library
crate, not a `tests/common/` module, because each integration binary is its own
compilation unit: as a module, every helper a given binary did not call looked
dead, which forced a blanket `#![allow(dead_code)]` that also hid genuinely dead
helpers. A library's `pub` surface is exempt from the lint, while the crate's own
private helpers stay checked. It is a dev-dependency of `agent-gossip` and
depends on `agent-gossip` in turn — cargo permits the cycle because the back-edge
is a dev-dependency. Note `bin()` resolves the binary under test by walking up
from `current_exe()`: `env!("CARGO_BIN_EXE_agent-gossip")` is defined only for
the owning package's integration tests, never for a library.

Three layers:
- **In-process (default, fast):** behavioral + output-schema tests drive the
  real event loop via the library `api` (`agent_gossip_test_fixtures::InProcNode`).
  Real iroh mesh, no subprocess — sub-second.
- **Every-run subprocess:** the wire-contract suite (CLI / stdout /
  `--output json` / Unix-socket / MCP-stdio) plus reliability invariants that
  need real OS processes and signals (SIGKILL beacon migration, SIGSTOP/CONT
  heal recovery, anti-entropy backfill) — `crates/agent-gossip/tests/gossip_network.rs`.
- **Adversarial (`--features adversarial`, `crates/agent-gossip/tests/adversarial.rs`):** an
  in-process attacker injects crafted wire bytes a correct client never
  produces; defended cases pass, open-gap `#[should_panic]` tripwires go red
  the moment a gap is closed. `cargo task test`/`ci` enable the feature.

**No environment-variable config.** Every knob is a `const` in
`util::consts` (edit + commit to experiment). The few the suite must
vary per-run are **hidden CLI flags** (`#[arg(hide = true)]`, e.g.
`--alive-timeout-secs`, `--heal-interval-secs`, `--log-dir`). Only `RUST_LOG`
and `NO_COLOR` are read from the environment.

### Terminal output

`plug`, `unplug`, `man`, and `doctor` print a report for a human; every other
command is JSON-only, and a live skill parses each. Two invariants:

- **JSON is never colored.** Color lives only in `util::output` and `doctor`'s
  `render_human`, both written through `anstream` — it resolves color support
  per stream at write time, so a pipe gets plain bytes and an agent sees no
  escapes. Hence no `--color` flag, no `is_terminal()` call, no env read of our
  own. `output::json::emit` stays plain `std::io::stdout`.
- **stdout is the product, stderr is only errors.** The roster *is* `plug`'s
  output, so `status`/`status_warn` print to stdout; only `warn`/`error` go to
  stderr.

### Logging

Developer logs use `tracing`. Daemons (`create`/`join`) write to
`<log_dir>/<mesh_prefix>-<nick>.log` (default: the `agent-gossip/logs`
subdir of the OS temp dir; `--log-dir` overrides). **Message bodies are
redacted by default** so a log is safe to share; pass the hidden `--log-raw`
for local debugging only. The `--output json` stdout stream is the functional
agent API — always raw, a separate path from the file sink.

Every log line carries an explicit `target:`, one per subsystem:
`agent_habilis_mesh::{lookup,gossip,lifecycle,beacon,directory,messages}`
(`EnvFilter` prefix-matches). Override at runtime, e.g.
`RUST_LOG=agent_habilis_mesh::gossip=trace cargo run -- create`.

`agent-gossip` emits under its **own** targets — `agent_gossip::{a2a,directory}`
— never the engine's. Those are pinned separately, in `APP_LOG_PINS`
(`agent-gossip/src/lib.rs`), which `agent_gossip::log_filter()` appends to the
engine's list; `cargo task layering` forbids the engine from naming them, and
`tests/log_pins.rs` fails if a target is emitted without a pin.

**Write `target: "agent_habilis_mesh::<subsystem>"` on every new `tracing` call
in `agent-habilis-mesh`.** The targets deliberately match the engine's own
crate path, so a call sitting in the module that owns its subsystem is covered
by the default target too — belt and braces rather than a single point of
failure. Cross-module calls still need it written out: `reassembly` logs to the
`gossip` target, and `messages` lives at `logging::messages`, so neither is
covered by its module path.

Those directives are pinned by `logging::log_filter`, and the pins are what keep
the connectivity story at `info` in a release build, whose base level is
`error`. A line whose target matches no pin compiles, passes review, works in a
debug build, and is silently dropped from every optimized one. Not hypothetical:
the beacon and gossip-neighbour lines once lost their targets, which made
`test_join_after_creator_departed_with_surviving_member` (it asserts on those
lines) fail 100% under `--profile ci` while passing locally in dev, and left
shipped release binaries with no beacon diagnostics at all. Aligning the targets
with the module path is what turned that from a live footgun into a backstop.

Note `transport` has a `LOG_TARGET` but no pin, so its lines only appear when
`RUST_LOG` names them — deliberate, it is verbose.

In-process tests install no subscriber of their own; call
`agent_gossip_test_fixtures::init_test_tracing()` (every `InProcNode`
constructor already does) and they honour `RUST_LOG` like a daemon.

### Man pages

Two manuals, one source each:
- **`agent-gossip man`** — the manual in man-page form, embedded from
  `docs/manual.txt` via `include_str!`. Edit that file to change it.
- **roff man pages** (`man agent-gossip`) — `cargo task man` walks the clap tree
  (`agent_gossip::cli_command()`) through `clap_mangen` in-process; the
  dep lives only in the dev-only `tasks` crate, never the shipped `agent-gossip`.

### Releasing

`cargo-release` never publishes to crates.io and never pushes automatically.

1. `cargo task release minor` (or `patch`/`major`/version) — dry run.
2. `cargo task release minor --execute` — bumps `Cargo.toml`/`Cargo.lock`,
   commits `chore: release v<version>`, creates the annotated tag. No push.
3. `git push origin main --follow-tags` — pushing the tag triggers
   `.github/workflows/release.yml`, which builds the binaries and **updates
   the Homebrew formula itself**, then mirrors it to the
   `agent-habilis/homebrew-tap` repo (needs the `TAP_PUSH_TOKEN` Actions
   secret — a fine-grained PAT with contents read/write on that repo).
   No manual formula step.

## Code Style

- Prefer descriptive names over single-letter ones, but idiomatic Rust wins.
- Lints are enforced workspace-wide (`clippy::pedantic` + `clippy::cargo` +
  cherry-picked restriction lints). `cargo task lint`/`ci` run
  `cargo clippy --all-targets -- -D warnings`, so any warning fails CI.
- `min_ident_chars` rejects single-char identifiers — rename closure params
  (`|e|` → `|error|`, `|m|` → `|msg|`).
- Renaming a serde-serialized field needs `#[serde(rename = "…")]` to keep the
  wire format stable.

## Agent Restrictions

- **NEVER** run `git commit` unless the user explicitly asks for it in the
  current request. Otherwise, the human user makes the commits.
- **NEVER** run `git push`. All pushes are done by the human user.

<!-- rtk-instructions v2 -->
# RTK (Rust Token Killer) - Token-Optimized Commands

## Golden Rule

**Always prefix commands with `rtk`**. If RTK has a dedicated filter, it uses it. If not, it passes through unchanged. This means RTK is always safe to use.

**Important**: Even in command chains with `&&`, use `rtk`:
```bash
# ❌ Wrong
git add . && git commit -m "msg" && git push

# ✅ Correct
rtk git add . && rtk git commit -m "msg" && rtk git push
```

## RTK Commands by Workflow

### Build & Compile (80-90% savings)
```bash
rtk cargo build         # Cargo build output
rtk cargo check         # Cargo check output
rtk cargo clippy        # Clippy warnings grouped by file (80%)
rtk tsc                 # TypeScript errors grouped by file/code (83%)
rtk lint                # ESLint/Biome violations grouped (84%)
rtk prettier --check    # Files needing format only (70%)
rtk next build          # Next.js build with route metrics (87%)
```

### Test (60-99% savings)
```bash
rtk cargo test          # Cargo test failures only (90%)
rtk go test             # Go test failures only (90%)
rtk jest                # Jest failures only (99.5%)
rtk vitest              # Vitest failures only (99.5%)
rtk playwright test     # Playwright failures only (94%)
rtk pytest              # Python test failures only (90%)
rtk rake test           # Ruby test failures only (90%)
rtk rspec               # RSpec test failures only (60%)
rtk test <cmd>          # Generic test wrapper - failures only
```

### Git (59-80% savings)
```bash
rtk git status          # Compact status
rtk git log             # Compact log (works with all git flags)
rtk git diff            # Compact diff (80%)
rtk git show            # Compact show (80%)
rtk git add             # Ultra-compact confirmations (59%)
rtk git commit          # Ultra-compact confirmations (59%)
rtk git push            # Ultra-compact confirmations
rtk git pull            # Ultra-compact confirmations
rtk git branch          # Compact branch list
rtk git fetch           # Compact fetch
rtk git stash           # Compact stash
rtk git worktree        # Compact worktree
```

Note: Git passthrough works for ALL subcommands, even those not explicitly listed.

### GitHub (26-87% savings)
```bash
rtk gh pr view <num>    # Compact PR view (87%)
rtk gh pr checks        # Compact PR checks (79%)
rtk gh run list         # Compact workflow runs (82%)
rtk gh issue list       # Compact issue list (80%)
rtk gh api              # Compact API responses (26%)
```

### JavaScript/TypeScript Tooling (70-90% savings)
```bash
rtk pnpm list           # Compact dependency tree (70%)
rtk pnpm outdated       # Compact outdated packages (80%)
rtk pnpm install        # Compact install output (90%)
rtk npm run <script>    # Compact npm script output
rtk npx <cmd>           # Compact npx command output
rtk prisma              # Prisma without ASCII art (88%)
```

### Files & Search (60-75% savings)
```bash
rtk ls <path>           # Tree format, compact (65%)
rtk read <file>         # Code reading with filtering (60%)
rtk grep <pattern>      # Search grouped by file (75%). Format flags (-c, -l, -L, -o, -Z) run raw.
rtk find <pattern>      # Find grouped by directory (70%)
```

### Analysis & Debug (70-90% savings)
```bash
rtk err <cmd>           # Filter errors only from any command
rtk log <file>          # Deduplicated logs with counts
rtk json <file>         # JSON structure without values
rtk deps                # Dependency overview
rtk env                 # Environment variables compact
rtk summary <cmd>       # Smart summary of command output
rtk diff                # Ultra-compact diffs
```

### Infrastructure (85% savings)
```bash
rtk docker ps           # Compact container list
rtk docker images       # Compact image list
rtk docker logs <c>     # Deduplicated logs
rtk kubectl get         # Compact resource list
rtk kubectl logs        # Deduplicated pod logs
```

### Network (65-70% savings)
```bash
rtk curl <url>          # Compact HTTP responses (70%)
rtk wget <url>          # Compact download output (65%)
```

### Meta Commands
```bash
rtk gain                # View token savings statistics
rtk gain --history      # View command history with savings
rtk discover            # Analyze Claude Code sessions for missed RTK usage
rtk proxy <cmd>         # Run command without filtering (for debugging)
rtk init                # Add RTK instructions to CLAUDE.md
rtk init --global       # Add RTK to ~/.claude/CLAUDE.md
```

## Token Savings Overview

| Category | Commands | Typical Savings |
|----------|----------|-----------------|
| Tests | vitest, playwright, cargo test | 90-99% |
| Build | next, tsc, lint, prettier | 70-87% |
| Git | status, log, diff, add, commit | 59-80% |
| GitHub | gh pr, gh run, gh issue | 26-87% |
| Package Managers | pnpm, npm, npx | 70-90% |
| Files | ls, read, grep, find | 60-75% |
| Infrastructure | docker, kubectl | 85% |
| Network | curl, wget | 65-70% |

Overall average: **60-90% token reduction** on common development operations.
<!-- /rtk-instructions -->
