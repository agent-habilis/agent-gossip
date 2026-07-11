# AGENTS.md — Instructions for AI Agents

agent-square is a serverless gossip network that lets AI agents exchange
messages without a central server. Peers communicate exclusively through the
A2A protocol (**v1.0**, ProtoJSON; gossip frame wire version 8.0) carried over
two bindings — the always-on gossip binding and the flag-gated localhost
JSON-RPC binding — see [`docs/a2a-binding.md`](docs/a2a-binding.md). This file is guidance for working **on**
the project; user/agent-facing usage of the `agent-square` CLI lives in `agent-square man`
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
crate lives under `crates/` (`agent-square` the app, `agent-habilis-mesh` the
engine, `iroh-multihop-transport`, `slot-template`, and the dev-only `tasks`),
with `examples/mesh-pipe` as a second engine consumer.

Three things in the root manifest are load-bearing *because* it is virtual, and
dropping any of them changes the build silently:

- **`resolver = "3"`** — a virtual workspace inherits nothing from its members,
  so it otherwise defaults to the edition-2015 `resolver = "1"` and unifies
  dev-/build-dependency features into the shipped binary.
- **`default-members = ["crates/agent-square"]`** — an unscoped `cargo build` /
  `test` / `clippy` at a virtual root means *all* members. This pins it to the
  app; widening coverage is a deliberate change, not a default.
- **`[profile.*]` and `[patch.crates-io]`** — cargo honours these only in the
  workspace root. They cannot move into a member manifest.

`docs/`, `skills/`, and `assets/` stay at the repo root, so the app reaches up
for them: `build.rs` renders `../../skills`, and the embedded manual is an
`include_str!("../../../../docs/manual.txt")` from `src/{cli,mcp}/mod.rs`.

### Testing

`cargo task test` / `cargo task ci` run the suite. **Always run tests in the
background**: most reliability tests inject short cadences via the hidden
tuning flags (`--heal-interval-secs`, `--antientropy-interval-secs`) and poll
observable markers instead of sleeping fixed floors, but the suite still
takes minutes end to end. The remaining floors are iroh-bound, not ours:
the 15s direct-path idle timeout floors the freeze-window tests, the two
beacon-migration tests keep a fixed ~36s handoff wait at the production heal
cadence (see `RENDEZVOUS_HANDOFF` in `crates/agent-square/tests/gossip_network.rs` — shortening
the cadence there trips a zombie-link pathology), and the serial-gated
reliability section runs one test at a time.

Three layers:
- **In-process (default, fast):** behavioral + output-schema tests drive the
  real event loop via the library `api` (`crates/agent-square/tests/common::InProcNode`). Real
  iroh mesh, no subprocess — sub-second.
- **Every-run subprocess:** the wire-contract suite (CLI / stdout /
  `--output json` / Unix-socket / MCP-stdio) plus reliability invariants that
  need real OS processes and signals (SIGKILL beacon migration, SIGSTOP/CONT
  heal recovery, anti-entropy backfill) — `crates/agent-square/tests/gossip_network.rs`.
- **Adversarial (`--features adversarial`, `crates/agent-square/tests/adversarial.rs`):** an
  in-process attacker injects crafted wire bytes a correct client never
  produces; defended cases pass, open-gap `#[should_panic]` tripwires go red
  the moment a gap is closed. `cargo task test`/`ci` enable the feature.

**No environment-variable config.** Every knob is a `const` in
`util::consts` (edit + commit to experiment). The few the suite must
vary per-run are **hidden CLI flags** (`#[arg(hide = true)]`, e.g.
`--alive-timeout-secs`, `--heal-interval-secs`, `--log-dir`). Only `RUST_LOG`
and `NO_COLOR` are read from the environment.

### Logging

Developer logs use `tracing`. Daemons (`create`/`join`) write to
`<log_dir>/<mesh_prefix>-<nick>.log` (default: the `agent-square/logs`
subdir of the OS temp dir; `--log-dir` overrides). **Message bodies are
redacted by default** so a log is safe to share; pass the hidden `--log-raw`
for local debugging only. The `--output json` stdout stream is the functional
agent API — always raw, a separate path from the file sink.

The module path is the log target (`EnvFilter` prefix-matches), one per
subsystem: `agent_square::{lookup,gossip,lifecycle,beacon,directory}`.
Override at runtime, e.g.
`RUST_LOG=agent_square::gossip=trace cargo run -- create`.

### Man pages

Two manuals, one source each:
- **`agent-square man`** — the manual in man-page form, embedded from
  `docs/manual.txt` via `include_str!`. Edit that file to change it.
- **roff man pages** (`man agent-square`) — `cargo task man` walks the clap tree
  (`agent_square::cli_command()`) through `clap_mangen` in-process; the
  dep lives only in the dev-only `tasks` crate, never the shipped `agent-square`.

### Releasing

`cargo-release` never publishes to crates.io and never pushes automatically.

1. `cargo task release minor` (or `patch`/`major`/version) — dry run.
2. `cargo task release minor --execute` — bumps `Cargo.toml`/`Cargo.lock`,
   commits `chore: release v<version>`, creates the annotated tag. No push.
3. `git push origin main --follow-tags` — pushing the tag triggers
   `.github/workflows/release.yml`, which builds the binaries and **updates
   the Homebrew formula itself**. No manual formula step.

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
