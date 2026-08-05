# agent-gossip-test-fixtures

The shared harness behind every [`agent-gossip`](../agent-gossip) integration
suite: two node types, the `cli_*` command helpers, the timeout budgets, and the
polling primitives. Test code only — never compiled into the shipped binary.

## Why it is a crate and not `tests/common/`

Each integration binary is its own compilation unit, so a shared `mod common;`
makes every helper *that binary* does not personally call look dead. With
eighteen binaries of wildly different appetites — `man_contract` uses one
helper, `gossip_network` uses two dozen — that was unavoidable, and the blanket
`#![allow(dead_code)]` it forced also hid genuinely dead helpers.

A library's `pub` surface is exempt from the dead-code lint by construction. So
the suppression is gone, while the crate's own *private* helpers stay checked.

It is a `[dev-dependencies]` entry of `agent-gossip` and depends on
`agent-gossip` in turn. Cargo permits the cycle because the back-edge is a
dev-dependency.

> **`bin()` walks `current_exe()` on purpose.** The obvious
> `env!("CARGO_BIN_EXE_agent-gossip")` cannot live here: cargo defines that
> variable only while compiling the owning package's integration tests and
> benches — never for a library, not even a path dev-dependency of that same
> package. So `bin()` resolves the binary from the *running* test executable
> instead (libtest puts it at `<target>/<profile>/deps/<name>-<hash>`; the app
> sits one level up). This also guarantees the freshly built binary rather than
> a stale release build, whose output formats may differ.

## Two node types

**`InProcNode`** — the default, and the fast one. A real `api::MeshSession`
(real iroh endpoint, the real `daemon::run` loop on a background task) plus its
captured `OutputEvent` stream, all inside the test process. Coverage is
recorded, teardown is deterministic, and a test runs sub-second. Constructors
mirror the CLI: `create`, `create_with_nick`, `create_with_password`, `join`,
and friends. Accessors drain the event stream by shape — `events`,
`json_events`, `message_events`, `messages`, `inbound`, `tasks`, `changes`,
`presence_count`, `count_body`.

**`Node`** — a real subprocess, for the things in-process cannot reach: the
stdout/JSON wire contract, the Unix socket, MCP stdio, and OS signals.
`create` / `create_named` / `create_flags` / `create_args` / `join` /
`join_flags` / `join_args` spawn it; `sigint`, `kill`, `stop`, `cont`, and
`wait_exit` drive its lifecycle (SIGKILL beacon migration and SIGSTOP/CONT heal
recovery are only testable this way); `log_contents` / `log_tail` / `messages` /
`wait_ready` observe it.

Use `InProcNode` unless the test's subject *is* the process boundary.

## Helpers

The `cli_*` family shells out to `bin()` and returns parsed output:
`cli_message`, `cli_poll`, `cli_poll_long`, `cli_peers`, `cli_ping`,
`cli_task_create`, `cli_task_followup`, `cli_task_artifact`, `cli_task_status`,
`cli_channel_get`, `cli_channel_merge`, plus `ipc_raw` for a hand-written
socket line. `test_cmd()` builds the command with the per-test-process
`--log-dir` already applied, so a test run never writes into the operator's
default `agent-gossip/logs`.

**Wait on markers, not on clocks.** `wait_until(count_fn, target, timeout)` and
`wait_total` poll an observable count every `POLL` (250ms) until it reaches the
target. The budgets are named by what they measure, not by a guessed duration:
`CONNECT_TIMEOUT`, `MSG_TIMEOUT` (steady-state delivery), `RECOVERY_TIMEOUT`,
`BIG_BODY_TIMEOUT`. A `sleep` of a fixed floor is the thing these exist to
avoid.

`serial_guard()` gates the reliability section down to one test at a time.
`mdns_multicast_available()` is deliberately asymmetric — a successful send does
not prove discovery works, but failure on *both* address families proves the
host cannot do multicast at all, so it only ever skips a host that is certainly
incapable (a VPN capturing the default route, a runner image with no IPv6
route).

## Logging

In-process tests install no subscriber of their own. Every `InProcNode`
constructor calls `init_test_tracing()`, which routes the daemon's `tracing` to
the test's captured stdout when `RUST_LOG` is set — so an in-process failure is
debuggable exactly like a subprocess one:

```sh
RUST_LOG=fofoca::gossip=debug cargo test -p agent-gossip <test-name>
```

It is idempotent and silent when `RUST_LOG` is unset. Without it, every
diagnostic the engine emits is discarded, which is why one early flake had to be
chased by bisecting asserts instead of reading a log.

## Run the suites

There is nothing to run in this crate — it has no tests of its own. It exists to
be consumed:

```sh
cargo task test    # the full workspace suite (background it — minutes)
cargo test -p agent-gossip --test gossip_network
```

> The suite takes minutes end to end, and the remaining floors are iroh-bound
> rather than ours: a 15s direct-path idle timeout floors the freeze-window
> tests, and the beacon-migration tests keep a fixed ~36s handoff wait at the
> production heal cadence — shortening that cadence trips a zombie-link
> pathology.

## What it is not

Not published, not a public API, and not held to one. Panicking on a broken
invariant *is* the assertion here, which is why the crate blanket-expects
`missing_panics_doc`, `missing_errors_doc`, and `must_use_candidate`: a
`# Panics` section on every helper would document nothing and no caller benefits
from `#[must_use]`. Add helpers freely; just keep them `pub` so the dead-code
lint stays meaningful for the private ones.
