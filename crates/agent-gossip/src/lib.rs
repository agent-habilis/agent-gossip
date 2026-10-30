//! `agent-gossip` — a mesh for AI agents.
//!
//! This crate ships as **both** a binary (the `agent-gossip`
//! CLI / MCP server) and a library. The binary is a thin shim over
//! [`run_cli`](fn@run_cli); library consumers embed a mesh in-process via the
//! [`api`] module.
//!
//! ## The library API
//!
//! The public surface is deliberately tiny and **iroh-free**: the
//! [`api::MeshSession`] facade owns the event loop as an in-process
//! `tokio` task (no subprocess, no Unix-socket IPC) and hands back only
//! the protocol value types re-exported below. iroh version bumps stay
//! an internal detail.
//!
//! ```no_run
//! use agent_gossip::api::{JoinConfig, MeshSession};
//! use agent_gossip::MessageBody;
//!
//! # async fn run() -> anyhow::Result<()> {
//! let session = MeshSession::join(JoinConfig::new("<hash>".parse()?)).await?;
//! let mut rx = session.messages();
//! session.broadcast(MessageBody::try_new("hello")?).await?;
//! while let Ok(msg) = rx.recv().await {
//!     println!("{} : {}", msg.author, msg.body);
//! }
//! session.leave().await?;
//! # Ok(())
//! # }
//! ```

// Curated public protocol surface. These types live in the engine crate
// (`habilis-network`); re-exporting them from this crate root keeps the
// externally-visible `agent_gossip::` API stable across the engine split.
pub use habilis_network::embed::{Lane, Reach, RosterEntry, RosterSnapshot};
pub use habilis_network::protocol::{
    BodyError, Channel, IdError, InviteTicket, JoinTarget, LookupSet, MeshId, MeshIdError,
    MeshName, Message, MessageBody, MessageId, MessageKind, NameError, Nickname, NicknameError,
    PresenceSubtype, RelayLadder, RelayLadderError, RelaySelection, Shard, ShardGroup,
};
// Wire/runtime constants the external test + bench crates assert against; the
// rest of `util::consts` stays engine-internal.
pub use habilis_network::util::consts::{
    MAX_LOGICAL_BODY_BYTES, MAX_MESSAGE_SIZE, MAX_SHARD_TOTAL,
};
pub use habilis_network::util::logging::LogSink;
pub use habilis_network::util::mesh_prefix;

pub use self::a2a::surfaced_event::SurfacedEvent;
pub use self::a2a::{TaskId, TaskState};
pub use self::emission::{event_json, surfaced_event_json};
// The `api::MeshSession::peers` / `ping` return types. Iroh-free by
// construction (nicknames, counts, and two field-less enums), so re-exporting
// them keeps the roster readable without widening the surface.
pub use self::output_event::{OutputEvent, PingPeer, TaskGoneReason};

// Application-layer modules. The engine modules (protocol, gossip, daemon,
// …) live in the `habilis-network` crate; this crate re-exports the
// curated public protocol surface from there above. `a2a` is public on
// purpose — it is the agent-communication data model both bindings (gossip,
// local JSON-RPC) share, and embedders speak it directly.
/// The A2A data model and the gossip binding that carries it.
pub mod a2a;
pub(crate) mod cli;
pub(crate) mod emission;
pub(crate) mod mcp;
/// The typed events a daemon writes to its `--output json` stream.
pub mod output_event;
pub(crate) mod policy;
pub(crate) mod topic;
pub(crate) mod tunnel;

pub mod api;
/// Roster and health values that `status` and `doctor` report.
pub mod status;

// Feature-gated test-support API, outside the supported surface. These shims expose the engine's
// internals to the crate's own bench/adversarial suites. See harness/mod.rs.
#[cfg(any(feature = "bench", feature = "adversarial"))]
pub mod harness;

pub use self::app_log_pins::APP_LOG_PINS;
pub use self::cli_command::cli_command;
pub use self::ensure_mesh_runtime_dir::ensure_mesh_runtime_dir;
pub use self::ensure_runtime_base::ensure_runtime_base;
pub use self::flush_log_if_pending::flush_log_if_pending;
pub use self::install_log_sink::install_log_sink;
pub use self::log_filter::log_filter;
pub(crate) use self::register_build_version::register_build_version;
pub use self::run_cli::run_cli;
pub use self::runtime_base::runtime_base;
pub use self::version::version;

mod app_log_pins;
mod cli_command;
mod cli_error;
mod ensure_mesh_runtime_dir;
mod ensure_runtime_base;
mod flush_log_if_pending;
mod install_log_sink;
mod log_filter;
mod product;
#[cfg(test)]
pub(crate) mod proptest_support;
mod register_build_version;
mod run_cli;
mod runtime_base;
mod version;
pub use self::cli_error::CliError;
