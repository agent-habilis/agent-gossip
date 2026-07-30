//! The daemon's shared event loop — used by both `create` and `join`.
//!
//! The loop owns three kinds of work:
//!
//! - **External inputs**: stdin (interactive mode), IPC commands
//!   (`msg` / `poll`), and incoming gossip events.
//! - **Time-driven maintenance**: heartbeat keepalives, silence
//!   sweeps, gossip healer.
//! - **Shutdown**: ctrl-c / SIGTERM.
//!
//! `daemon` is orchestration + plumbing: the `select!` loop, IPC
//! command application (`ipc`), shared handler context (`ctx`),
//! in-memory accounting (`state`, `message_log`),
//! `config`, `setup`, housekeeping `timers`. The behavioral
//! subsystems are crate-root siblings, each its own `RUST_LOG`
//! target: `crate::gossip`, `crate::lifecycle`, `crate::beacon`,
//! `crate::lookup`.

pub(crate) mod app;
mod bounded_id_set;
pub(crate) mod config;
pub(crate) mod ctx;
pub(crate) mod node;
// In-memory accounting stores owned by `EventLoopState`. `pub(crate)` so
// the gossip anti-entropy layer (and its tests) can name `MessageLog` /
// `DigestWindow`; still crate-internal.
pub(crate) use crate::doc;
pub(crate) mod message_log;
pub(crate) mod params;
// Dedicated, byte-budgeted buffer for partial multipart bodies — reassembly
// no longer reads the message log, so log eviction can't break it.
pub(crate) use crate::reassembly;
pub(crate) mod setup;
pub(crate) mod state;

// The session state file the daemon writes for external readers (its
// sole writer). Daemon-session state, not a generic `util` helper.
pub(crate) mod state_file;
pub(crate) mod timers;

pub(crate) mod event_loop;

// Crate-internal shorthands. The public spelling of all of these is
// `crate::runtime`, which is what a consumer imports.
pub(crate) use config::{CoHostPolicy, DriverMode, EventLoopConfig};
pub(crate) use event_loop::run;
