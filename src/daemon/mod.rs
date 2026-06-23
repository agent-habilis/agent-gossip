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

mod bounded_id_set;
mod config;
pub(crate) mod ctx;
pub(crate) mod ipc;
// In-memory accounting stores owned by `EventLoopState`. `pub(crate)` so
// the gossip anti-entropy layer (and its tests) can name `MessageLog` /
// `DigestWindow`; still crate-internal.
pub(crate) mod message_log;
pub(crate) mod state_log;
pub(crate) mod params;
// Normally private to the daemon; widened to `pub(crate)` only under the
// `bench` feature so `harness::bench` can reach `SwarmRateLimiter`. Non-bench
// builds keep the original surface.
#[cfg(feature = "bench")]
pub(crate) mod rate_limit;
#[cfg(not(feature = "bench"))]
mod rate_limit;
pub(crate) mod setup;
pub(crate) mod state;
// Local, seq-ordered record of surfaced events — the `poll` / `fetch_messages`
// history. Distinct from `message_log` (the cross-node anti-entropy buffer).
pub(crate) mod surfaced;
// The session state file the daemon writes for external readers (its
// sole writer). Daemon-session state, not a generic `util` helper.
pub(crate) mod exchange;
mod state_file;
pub(crate) mod timers;

mod event_loop;

pub(crate) use config::{CoHostPolicy, DriverMode, EventLoopConfig, SessionRequest};
pub(crate) use event_loop::run;
pub(crate) use params::{CreateParams, JoinParams, Resolved};
