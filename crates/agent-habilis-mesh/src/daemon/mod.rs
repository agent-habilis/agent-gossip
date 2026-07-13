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

pub mod app;
mod bounded_id_set;
pub mod config;
pub mod ctx;
pub mod node;
// In-memory accounting stores owned by `EventLoopState`. `pub(crate)` so
// the gossip anti-entropy layer (and its tests) can name `MessageLog` /
// `DigestWindow`; still crate-internal.
pub use crate::doc;
pub(crate) mod message_log;
pub(crate) mod params;
// Dedicated, byte-budgeted buffer for partial multipart bodies — reassembly
// no longer reads the message log, so log eviction can't break it.
pub use crate::reassembly;
pub mod setup;
pub mod state;
pub use crate::doc::wire as state_doc;
// The session state file the daemon writes for external readers (its
// sole writer). Daemon-session state, not a generic `util` helper.
pub mod state_file;
pub(crate) mod timers;

pub(crate) mod event_loop;

pub use config::{CoHostPolicy, DriverMode, EventLoopConfig, ReadyAnnounce};
pub use event_loop::run;
pub use node::Node;
pub use params::{CreateParams, JoinParams, Resolved, TopicParams, derive_topic_mesh};
