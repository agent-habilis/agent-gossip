//! `agent-habilis-swarm` — a mesh for AI agents.
//!
//! This crate ships as **both** a binary (the `ahs`
//! CLI / MCP server) and a library. The binary is a thin shim over
//! [`run_cli`]; library consumers embed a swarm in-process via the
//! [`embed`] module.
//!
//! ## Embedding
//!
//! The public surface is deliberately tiny and **iroh-free**: the
//! [`embed::SwarmSession`] facade owns the event loop as an in-process
//! `tokio` task (no subprocess, no Unix-socket IPC) and hands back only
//! the protocol value types re-exported below. iroh version bumps stay
//! an internal detail.
//!
//! ```no_run
//! use agent_habilis_swarm::embed::{JoinConfig, SwarmSession};
//! use agent_habilis_swarm::MessageBody;
//!
//! # async fn run() -> anyhow::Result<()> {
//! let session = SwarmSession::join(JoinConfig::new("ahs...")).await?;
//! let mut rx = session.messages();
//! session.send(MessageBody::new("hello")?, None).await?;
//! while let Ok(msg) = rx.recv().await {
//!     println!("{} : {}", msg.author, msg.body);
//! }
//! session.leave().await?;
//! # Ok(())
//! # }
//! ```

// Internal modules stay `pub(crate)`: the curated public surface is
// the `embed` facade plus the protocol re-exports below. Keeping these
// crate-private means iroh / internal refactors are never breaking
// public API changes.
pub(crate) mod beacon;
pub(crate) mod cli;
pub(crate) mod daemon;
pub(crate) mod discovery;
pub(crate) mod gossip;
pub(crate) mod lifecycle;
pub(crate) mod logsink;
pub(crate) mod mcp;
pub(crate) mod messages;
pub(crate) mod output;
pub(crate) mod protocol;
pub(crate) mod resolver;
pub(crate) mod transport;
pub(crate) mod util;

pub mod embed;

// Curated public protocol surface. These types are `pub` inside their
// (otherwise `pub(crate)`) modules; re-exporting them from the crate
// root is what makes them externally reachable and satisfies
// `unreachable_pub`.
pub use logsink::LogSink;
pub use output::{OutputEvent, event_json};
pub use protocol::message::{
    BodyError, IdError, Message, MessageBody, MessageId, MessageKind, PresenceSubtype,
};
pub use protocol::nickname::{Nickname, NicknameError};
pub use protocol::swarm::{SwarmId, SwarmIdError};

use anyhow::Result;
use clap::Parser;

use cli::Cli;

/// Parse `argv` and run the selected CLI subcommand to completion.
///
/// This is the entire body of the `ahs` binary; it is
/// public so the thin `src/main.rs` shim (which owns only
/// process-level concerns: tracing init, terminal echo) can call it.
/// The subcommand dispatch + per-command logic lives in [`cli`].
///
/// # Errors
/// Propagates any error from the selected subcommand — swarm setup
/// failure, join timeout, IPC errors, invalid swarm-mode flags, etc.
pub async fn run_cli() -> Result<()> {
    cli::dispatch(Cli::parse()).await
}

/// Build the deferred log sink and register it process-globally.
/// Call once in `main` before subscriber init; pass the returned
/// value to `tracing_subscriber::fmt().with_writer(..)`. Logs buffer
/// until [`cli`] resolves the swarm id + nickname (see `logsink`).
#[must_use]
pub fn install_log_sink() -> LogSink {
    logsink::install()
}

/// Flush buffered logs to stderr if identity was never resolved
/// (transient command, or startup failed before attach). Call after
/// `run_cli` returns.
pub fn flush_log_if_pending() {
    logsink::flush_pending_to_stderr();
}
