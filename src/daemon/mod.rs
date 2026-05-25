//! The daemon's shared event loop — used by both `create` and `join`.
//!
//! The loop owns three kinds of work:
//!
//! - **External inputs**: stdin (interactive mode), IPC commands
//!   (`msg` / `poll`), and incoming gossip events.
//! - **Time-driven maintenance**: heartbeat keepalives, silence
//!   sweeps, gossip healer, rate-limit pruning.
//! - **Shutdown**: ctrl-c / SIGTERM.
//!
//! `daemon` is orchestration + plumbing: the `select!` loop, IPC
//! command application (`ipc`), shared handler context (`ctx`),
//! in-memory accounting (`state`, `message_log`, `rate_limit`),
//! `config`, `setup`, housekeeping `timers`. The behavioral
//! subsystems are crate-root siblings, each its own `RUST_LOG`
//! target: `crate::gossip`, `crate::lifecycle`, `crate::beacon`,
//! `crate::lookup`.

mod config;
pub(crate) mod ctx;
pub(crate) mod ipc;
// In-memory accounting stores owned by `EventLoopState`. Private to
// `daemon` — no consumer outside the event loop.
mod message_log;
pub(crate) mod params;
mod rate_limit;
pub(crate) mod setup;
pub(crate) mod state;
// The session state file the daemon writes for external readers (its
// sole writer). Daemon-session state, not a generic `util` helper.
mod state_file;
pub(crate) mod timers;

mod event_loop;

pub(crate) use config::{CoHostPolicy, DriverMode, EventLoopConfig, SessionRequest};
pub(crate) use event_loop::run;
pub(crate) use params::{CreateParams, JoinParams, Resolved};
