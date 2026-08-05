# tasks

The project's task runner. Every development operation in this repo goes through
`cargo task <name>`; `cargo task` with no arguments lists them all.

```toml
# .cargo/config.toml
[alias]
task = "run --package tasks --"
```

## Why it exists

The commands here are not thin aliases — most of them carry a flag or an
argument order that is load-bearing, and the *reason* it is load-bearing sits in
a comment next to it. A few examples of what a bare `cargo test` gets wrong:

- **`test` runs twice at different parallelism.** The subprocess reliability
  suite (SIGSTOP storms, beacon migration, anti-entropy) starves itself at `-j4`
  because the recovery windows contend, so it wants `-j2`. The in-process
  adversarial suite is the reverse — it needs the worker threads higher
  parallelism provides to keep its mesh responsive, and flakes at `-j2`. So:
  everything except adversarial at 2, then the adversarial suite alone at 4.
- **`test` and `lint` are `--workspace`, not `-p agent-gossip`.** The root
  manifest is virtual and pins `default-members` to the app, so an unscoped
  invocation would silently skip the engine — which owns the wire version, every
  crypto byte-domain, and `runtime_base()`. Scoped to the app, none of that was
  ever tested and a stale engine snapshot stayed green.
- **`lint` passes `--features agent-gossip/adversarial`.** The adversarial suite
  is `required-features`-gated, so clippy skips it otherwise. The feature stays
  *qualified* because a bare feature name is ambiguous in a workspace-wide
  invocation.
- **`install` needs both `--force` and `--locked`.** Without `--force`,
  `cargo install` reads the unchanged crate version as "already up to date" and
  skips the rebuild, which shipped a stale binary to fleet hosts whose git-hash
  stamp lagged the checked-out commit. Without `--locked` it re-resolves on the
  host, and a registry release after the lock was cut can break the build.

Encoding those in a runner means nobody has to remember them, and the
justification travels with the command.

## The tasks

| Task | What it does |
|---|---|
| `test` | The suite. Two runs at different `--test-threads`, `--no-fail-fast`, then prunes stale `target/` artifacts |
| `ci` | The full gate: `fmt --check` → `layering` → clippy → tests |
| `lint` | `cargo clippy --workspace --all-targets -- -D warnings` |
| `layering` | Fails if the engine crate names A2A or the application (see AGENTS.md) |
| `fmt` | `cargo fmt --all` |
| `proptest` | The property-based tests only (`prop_`) |
| `coverage` | `cargo llvm-cov`, installing it on demand |
| `bench` | Everything; `bench transfer` for the loopback soak alone; any other arg is a divan filter (e.g. `bench derive_secret`) |
| `build` | The binary. Cross-compiles with `--target <triple>` or the `--arch` shorthand |
| `run` | `cargo run --` with the rest forwarded (`cargo task run create`) |
| `install` | `cargo install` from an absolute path, then reports what the installed binary *says* its version is |
| `release` | `cargo-release`; dry run unless `--execute`. With no args, just builds the release binary |
| `man` | Renders roff man pages into `target/man/` |
| `logs` | Prints the daemon log directory, creating it if missing |
| `clean` | `cargo clean` plus the separate llvm-cov target dir |

The `Task` enum variants in [`src/main.rs`](src/main.rs) are the source of
truth, and their `///` doc comments *are* the `--help` text — there is no
separate usage block to drift. clap kebab-cases the names, so the invocation
surface is stable.

There is also a hidden `zig` subcommand. It is not for human use: cargo-zigbuild's
cross-link wrapper re-execs *this* binary as `<exe> zig …` (it resolves itself
through `current_exe()`), and for the archiver step it copies this executable to
`ar`/`lib`/`dlltool` and dispatches on `argv[0]`. Both paths are handled in
`main`, and the cross build in `build` can only link because they exist.

## Dependencies used as libraries, not installs

Nothing here asks the developer to `cargo install` a toolchain first.

- **`cargo-zigbuild`** is a normal crate dependency, driven as a library:
  `build` calls `Build::execute()` directly. zig itself is vendored into
  `target/tooling/` on first use at a pinned version — never the dev's global or
  brew zig — so a cross build is self-contained and reproducible. `--arch` is
  sugar for a static-musl Linux target, the shape the Raspberry Pi fleet
  deploys.
- **`clap_mangen`** is what makes `man` work in-process: it walks the app's clap
  tree via `agent_gossip::cli_command()` and emits one page per subcommand. This
  is why the task runner depends on [`agent-gossip`](../agent-gossip) — and why
  the mangen dependency lives *here* and never in the shipped binary.
- **`fofoca`** is a dependency for one reason: `util::output`, the
  engine's cargo-style status helpers, reused rather than forked. Both audiences
  want the same colored lines, and `anstream` already strips the color for
  whichever of them is piping.
- **`cargo-llvm-cov`, `cargo-release`, and `cargo-sweep`** are installed on
  demand by `ensure_installed`, which probes first and never aborts the calling
  task on a hiccup.

## Releasing

`cargo-release` never publishes to crates.io and never pushes.

```sh
cargo task release minor              # dry run
cargo task release minor --execute    # bump, commit, annotated tag — no push
git push origin main --follow-tags    # the human does this
```

Pushing the tag triggers `.github/workflows/release.yml`, which builds the
binaries, **updates the Homebrew formula itself**, and mirrors it to the tap
repo. There is no manual formula step.

## What it is not

Not published, not installed, and not shipped to anyone — it is a workspace
member that exists only to be run through the `cargo task` alias. It is also not
a general build system: it wraps cargo, and every task is a handful of `cmd!`
invocations plus the comment explaining the flags. Keep it that way; logic that
grows past that belongs in the code being built.
