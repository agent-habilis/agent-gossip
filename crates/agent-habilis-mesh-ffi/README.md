# agent-habilis-mesh-ffi

A C ABI over the [`agent-habilis-mesh`](../agent-habilis-mesh) engine — the
**third consumer**, and the first that is not written in Rust. A C program links
one shared library, calls a dozen functions, and is a full member of a gossip
mesh: it creates or joins, exchanges broadcast and directed messages, and reads
and writes the shared JSON state document. No daemon, no socket, no subprocess —
the event loop runs on a tokio runtime inside the caller's own process.

The C consumer that drives it lives in
[`examples/mesh-pipe-c`](../../examples/mesh-pipe-c).

## Why it is a separate crate

The engine cannot expose this itself. A `cdylib` crate-type cannot be switched on
per-feature, so declaring one on `agent-habilis-mesh` would make every build of
the engine — including the one inside the shipped `agent-gossip` binary — pay for
a dynamic library nobody links. The ABI also wants a lint posture the engine
does not: the workspace denies `unsafe_code`, and this crate is nothing *but*
raw pointers from a foreign caller.

Like `mesh-pipe`, it depends on the engine and nothing else. It never touches
`agent-gossip` or the A2A data model.

## The surface

[`include/mesh.h`](include/mesh.h) is the hand-written, committed declaration —
the source of truth for a C caller, and the counterpart of
[`src/ffi.rs`](src/ffi.rs). Change one, change the other.

```c
mesh_pipe *pipe = mesh_open(&(mesh_opts){ .is_public = 1 });
printf("joined %s as %s\n", mesh_id(pipe), mesh_nickname(pipe));

while (mesh_peer_count(pipe) < 1) { /* wait for company */ }

mesh_send(pipe, NULL, (const uint8_t *)"hello", 5);   /* broadcast */
mesh_send(pipe, "bob", (const uint8_t *)"psst", 4);   /* just bob */
mesh_state_merge(pipe, "{\"phase\":\"ready\"}");        /* shared JSON state */

mesh_frame frame;
if (mesh_recv(pipe, buf, mesh_max_chunk(), 5000, &frame) > 0) { /* … */ }

mesh_close(pipe);
```

Three conventions hold across the whole surface:

- **Failures are quiet in the return value, loud in the error slot.** A
  pointer-returning call yields NULL, an `int` call returns `-1`, and
  `mesh_last_error()` says why. A call clears the slot on entry, so it only ever
  holds the most recent outcome.
- **Buffers are sized by asking.** `mesh_state_json` / `mesh_peers_json` return
  the length the document needs; pass a NULL buffer to ask, then call again with
  one that fits. `mesh_recv` refuses a buffer smaller than the frame rather than
  truncating — size it with `mesh_max_chunk()`.
- **A handle belongs to one thread.** Distinct handles are fully independent, so
  one process can hold several members of the same mesh (which is exactly what
  the C test does).

`mesh_peer_count` is worth one note: it counts peers **other than you**, so a
lone member reads `0`, while the roster JSON's own `count` field includes you and
reads `1`. The loop above is why it exists — nothing is retained, so a sender
with no peers is not early, it is throwing bytes away.

## Two things that will bite you

- **No signal handlers.** `mesh-pipe` passes `handle_signals: true` to
  `Node::spawn` because it owns its process; this crate passes `false`. A library
  that installs process-wide ctrl-c / SIGTERM listeners hijacks its host's own
  handling. A C caller traps signals itself and calls `mesh_close`.
- **`src/pipe.rs` has a twin.** Its frame taxonomy (`pipe_data` / `pipe_eof`, the
  base64 body, the `AppClass` flags, the chunk budget) duplicates
  `examples/mesh-pipe/src/main.rs` on purpose — hoisting it here would cost
  `mesh-pipe` its "depends on the engine and nothing else" property. The
  duplication is held together by the cross-language roundtrip in
  `tests/c_suite.rs`, not by discipline.

## Test it

```bash
cargo test -p agent-habilis-mesh-ffi
```

Two layers:

- **`tests/ffi_smoke.rs`** drives the `extern "C"` functions from Rust through
  the crate's `rlib`. It is about the boundary — NULL handling, the error slot,
  the buffer conventions — and needs no C compiler, so the ABI stays covered on a
  host that cannot build the C example.
- **`tests/c_suite.rs`** compiles the programs in `examples/mesh-pipe-c` against
  the cdylib and runs them: the four-scenario C suite, plus a byte-for-byte
  roundtrip in **both** directions between the C program and the Rust `mesh-pipe`
  binary. Both skip loudly (never silently) when there is no usable `cc`, or when
  `mesh-pipe` has not been built.

## What it is not

Not a published crate (`publish = false`), not a stable ABI, and not the way to
drive `agent-gossip` from another language — that is the a2a JSON-RPC binding,
which [`examples/a2a-interop`](../../examples/a2a-interop) shows from TypeScript.
This is the *engine*: raw frames, no A2A semantics, no agent card.
