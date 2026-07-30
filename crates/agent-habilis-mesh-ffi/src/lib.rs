//! `agent-habilis-mesh-ffi` — the C-ABI boundary over the `agent-habilis-mesh`
//! engine.
//!
//! The **third consumer** of the engine, and the first non-Rust one: it exposes
//! an opaque handle plus blocking byte/JSON calls (see `include/mesh.h`) so a C
//! program can create or join a mesh, exchange broadcast and directed messages,
//! and read/write the shared automerge state document — in-process, with no
//! daemon and no socket.
//!
//! The [`pipe`] module keeps `examples/mesh-pipe`'s `pipe_data` / `pipe_eof`
//! frame taxonomy, so the C consumer and the Rust `mesh-pipe` binary talk to
//! each other over the same mesh; [`ffi`] is the thin unsafe shim over it.

pub mod ffi;
pub mod pipe;
