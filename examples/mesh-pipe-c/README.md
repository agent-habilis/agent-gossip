# mesh-pipe-c

[`mesh-pipe`](../mesh-pipe), in C. Same subcommands, same flags, same wire
frames — so either binary talks to the other over one mesh. It is a consumer of
[`agent-habilis-mesh-ffi`](../../crates/agent-habilis-mesh-ffi), and the proof
that the engine's application seam works from outside Rust.

Two programs:

- **`pipe.c`** → `mesh-pipe-c`, the pipe itself.
- **`ffi_test.c`** → `ffi_test`, the C-side test of the FFI (below).

## Build it

```bash
make          # builds the Rust cdylib first, then both binaries
make check    # runs ffi_test
make clean
```

`make` shells out to `cargo build -p agent-habilis-mesh-ffi`. The `-p` is
load-bearing: the workspace pins `default-members` to the `agent-gossip` app, so
a bare `cargo build` at the root never builds the FFI crate.

This is plain C plus a Makefile — deliberately *not* a cargo crate, so nothing
here is a workspace member. The Rust side of the tests lives in
`crates/agent-habilis-mesh-ffi/tests/c_suite.rs`, which compiles these same
sources so `cargo test` covers them.

## Run it

```bash
# terminal 1 — send; prints the mesh id on stderr
./mesh-pipe-c listen < some-file

# terminal 2 — receive
./mesh-pipe-c connect '💬…' > copy
```

Mix the languages freely; the frames are identical, so either side can be the
Rust binary:

```bash
./mesh-pipe-c listen < some-file            # C sends
cargo run -p mesh-pipe -- connect '💬…'      # Rust receives

cargo run -p mesh-pipe -- listen < some-file # Rust sends
./mesh-pipe-c connect '💬…'                  # C receives
```

Both print the minted id as `mesh-pipe: mesh 💬…` — the same prefix from both
binaries, deliberately, because that line is the machine-readable handoff the
test harness parses. Diagnostics are prefixed `mesh-pipe-c:` so you can still
tell which one is talking.

`connect` also takes `--idle-timeout SECS`, which the Rust binary has no need
for: it lets a receiver give up rather than block forever when the sender dies.

## Why `listen` waits

`listen` prints the mesh id and then waits — up to `--wait-for-peer SECS`,
default 120 — for someone to join before it reads a byte of stdin. Without that
wait, `./mesh-pipe-c listen < file` is over in about two seconds: `fread` returns
the whole file at once, the frames go out to an empty mesh, and the process says
goodbye long before a human can paste the id anywhere. The payload is lost and
nothing reports an error.

Lingering *after* sending would not have fixed it. Pipe frames are classified
`loggable: false, chained: false` and the app returns `false` from
`on_app_frame`, so the engine never retains them — anti-entropy has nothing to
backfill a late joiner with. The only place the wait can go is before the send.

If no one arrives inside the window, `listen` exits **non-zero**: nothing was
delivered, and a pipe that exits 0 while dropping its input is the bug this
guards against. `--wait-for-peer 0` restores the old send-immediately behaviour
for a script that knows its peer is already there.

## What ffi_test covers

Three memberships in **one process** — each handle owns its own runtime and
endpoint — over a private loopback mesh. No network, nothing to clean up.

1. **Creates and joins.** `alice` creates; `bob` and `carol` join by id; the test
   waits until each shows up in the others' roster.
2. **Syncs JSON state.** `alice` merges into the shared state document and `bob`
   converges on it; then `bob` merges and `alice` converges. The second leg also
   asserts `alice`'s own key survived `bob`'s write — this is a CRDT, not
   last-writer-wins.
3. **Broadcasts.** `alice` broadcasts; `bob` receives the exact bytes, attributed
   to `alice`, not marked directed.
4. **Sends to one peer.** `bob` sends to `alice` by nickname; `alice` receives it
   marked directed, and the whisper never reaches `carol`. The negative leg is the
   part that actually distinguishes "directed" from "broadcast"; it asserts the
   frame is not *surfaced* to `carol`, not that no bytes reached her host.

   That leg checks "the whisper never arrives", not "nothing arrives" — `carol` is
   also a recipient of the scenario-3 broadcast, and her copy can still be in
   flight. Draining her queue first only looked deterministic: a late broadcast
   landing inside the window failed the test for the wrong reason.

Output is `ok N - …` per scenario, non-zero exit at the first failure with the
reason (and, for a timeout, the last document or roster it saw) on stderr.

## Portability

macOS and Linux. The Makefile picks the right shared-library extension and bakes
an absolute rpath, so the binaries run from any directory. It compiles with
`-std=gnu11` rather than `-std=c11` because strict ANSI hides the POSIX
declarations these programs use (`sigaction`, `clock_gettime`, `usleep`) behind
feature macros.
