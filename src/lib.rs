//! `agent-habilis-swarm` — a mesh for AI agents.
//!
//! This crate ships as **both** a binary (the `ahsw`
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
//! let session = SwarmSession::join(JoinConfig::new("🐝...".parse()?)).await?;
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
pub(crate) mod directory;
pub(crate) mod file;
pub(crate) mod gossip;
pub(crate) mod lifecycle;
pub(crate) mod logging;
pub(crate) mod lookup;
pub(crate) mod mcp;
pub(crate) mod output;
pub(crate) mod pipe;
pub(crate) mod port;
pub(crate) mod protocol;
pub(crate) mod resolver;
pub(crate) mod sh;
pub(crate) mod transport;
pub(crate) mod util;

pub mod embed;

// Not public API. Feature-gated, doc-hidden shims that expose pub(crate)
// internals to the crate's own bench/adversarial suites. See harness/mod.rs.
#[cfg(any(feature = "bench", feature = "adversarial"))]
#[doc(hidden)]
pub mod harness;

// Curated public protocol surface. These types are `pub` inside their
// (otherwise `pub(crate)`) modules; re-exporting them from the crate
// root is what makes them externally reachable and satisfies
// `unreachable_pub`.
pub use daemon::surfaced::SurfacedEvent;
pub use logging::LogSink;
pub use output::{OutputEvent, event_json, surfaced_event_json};
pub use protocol::message::{
    BodyError, Channel, ExchangeId, ExchangeIdError, ExchangeKind, ExchangeKindError,
    ExchangePhase, ExchangePhaseError, IdError, Message, MessageBody, MessageId, MessageKind, Part,
    PartGroup, PresenceSubtype,
};
pub use protocol::nickname::{Nickname, NicknameError};
pub use protocol::swarm::{
    LookupSet, NameError, RelayLadder, RelayLadderError, RelaySelection, SwarmId, SwarmIdError,
    SwarmName,
};
pub use resolver::{JoinTarget, JoinTargetError};
// Wire/runtime constants the external test + bench crates assert against; the
// rest of `util::consts` stays crate-internal.
pub use util::consts::{MAX_LOGICAL_BODY_BYTES, MAX_MESSAGE_PARTS, MAX_MESSAGE_SIZE, RUNTIME_DIR};
pub use util::swarm_prefix;
pub use util::version::VERSION;

use anyhow::Result;
use clap::Parser;

use cli::Cli;

/// Parse `argv` and run the selected CLI subcommand to completion.
///
/// This is the entire body of the `ahsw` binary; it is
/// public so the thin `src/main.rs` shim (which owns only
/// process-level concerns: tracing init, terminal echo) can call it.
/// The subcommand dispatch + per-command logic lives in `cli`.
///
/// # Errors
/// Propagates any error from the selected subcommand — swarm setup
/// failure, join timeout, IPC errors, invalid swarm-mode flags, etc.
pub async fn run_cli() -> Result<()> {
    // Box the single large await so it lives on the heap, not this frame —
    // the same lever the event loop uses (see `daemon::event_loop`); the
    // setup → run future is near clippy's `large_futures` budget.
    Box::pin(cli::dispatch(Cli::parse())).await
}

/// The fully-built `ahsw` clap command tree, for offline man-page
/// generation (`cargo task man` walks it in-process through
/// `clap_mangen`). Arg surface only; no iroh, no runtime state.
#[must_use]
pub fn cli_command() -> clap::Command {
    <Cli as clap::CommandFactory>::command()
}

/// Build the deferred log sink and register it process-globally.
/// Call once in `main` before subscriber init; pass the returned
/// value to `tracing_subscriber::fmt().with_writer(..)`. Logs buffer
/// until `cli` resolves the swarm id + nickname (see `logging`).
#[must_use]
pub fn install_log_sink() -> LogSink {
    logging::install()
}

/// The default tracing directive filter; pass to
/// `tracing_subscriber::fmt().with_env_filter(..)`. `RUST_LOG` overrides
/// it. See `logging`.
#[must_use]
pub fn log_filter() -> tracing_subscriber::EnvFilter {
    logging::log_filter()
}

/// Flush buffered logs to stderr if identity was never resolved
/// (transient command, or startup failed before attach). Call after
/// `run_cli` returns.
pub fn flush_log_if_pending() {
    logging::flush_pending_to_stderr();
}

// Shared config for the crate's `proptest!` blocks. Overrides the default
// failure-persistence path so regression seeds land in
// `tests/proptest-regressions` rather than a `proptest-regressions` folder
// at the crate root. Every `proptest!` block opts in with
// `#![proptest_config(crate::proptest_support::config())]`. Kept last so it
// does not trip `clippy::items_after_test_module`.
#[cfg(test)]
pub(crate) mod proptest_support {
    use proptest::test_runner::{Config, FileFailurePersistence};

    pub(crate) fn config() -> Config {
        Config {
            failure_persistence: Some(Box::new(FileFailurePersistence::SourceParallel(
                "tests/proptest-regressions",
            ))),
            ..Config::default()
        }
    }
}
