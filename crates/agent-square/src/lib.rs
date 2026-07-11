//! `agent-square` — a mesh for AI agents.
//!
//! This crate ships as **both** a binary (the `agent-square`
//! CLI / MCP server) and a library. The binary is a thin shim over
//! [`run_cli`]; library consumers embed a mesh in-process via the
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
//! use agent_square::api::{JoinConfig, MeshSession};
//! use agent_square::MessageBody;
//!
//! # async fn run() -> anyhow::Result<()> {
//! let session = MeshSession::join(JoinConfig::new("💬...".parse()?)).await?;
//! let mut rx = session.messages();
//! session.send(MessageBody::new("hello")?).await?;
//! while let Ok(msg) = rx.recv().await {
//!     println!("{} : {}", msg.author, msg.body);
//! }
//! session.leave().await?;
//! # Ok(())
//! # }
//! ```

// Application-layer modules. The engine modules (protocol, gossip, daemon,
// …) live in the `agent_habilis_mesh` crate; this crate re-exports the
// curated public protocol surface from there below. `a2a` is public on
// purpose — it is the agent-communication data model both bindings (gossip,
// local JSON-RPC) share, and embedders speak it directly.
pub mod a2a;
pub(crate) mod cli;
pub(crate) mod mcp;
pub(crate) mod output;

pub mod api;

// Not public API. Feature-gated, doc-hidden shims that expose the engine's
// internals to the crate's own bench/adversarial suites. See harness/mod.rs.
#[cfg(any(feature = "bench", feature = "adversarial"))]
#[doc(hidden)]
pub mod harness;

// Curated public protocol surface. These types live in the engine crate
// (`agent_habilis_mesh`); re-exporting them from this crate root keeps the
// externally-visible `agent_square::` API stable across the engine split.
pub use a2a::surfaced::SurfacedEvent;
pub use a2a::{TaskId, TaskState};
// The `api::MeshSession::peers` / `ping` return types. Iroh-free by
// construction (nicknames, counts, and two field-less enums), so re-exporting
// them keeps the roster readable without widening the surface.
pub use agent_habilis_mesh::daemon::state::{Reach, RosterEntry, RosterSnapshot};
pub use agent_habilis_mesh::invite::InviteTicket;
pub use agent_habilis_mesh::logging::LogSink;
pub use agent_habilis_mesh::protocol::mesh::{
    LookupSet, MeshId, MeshIdError, MeshName, NameError, RelayLadder, RelayLadderError,
    RelaySelection,
};
pub use agent_habilis_mesh::protocol::message::{
    BodyError, Channel, IdError, Message, MessageBody, MessageId, MessageKind, PresenceSubtype,
    Shard, ShardGroup,
};
pub use agent_habilis_mesh::protocol::nickname::{Nickname, NicknameError};
pub use agent_habilis_mesh::resolver::{JoinTarget, JoinTargetError};
pub use agent_habilis_mesh::transport::TransportPolicy;
pub use agent_habilis_mesh::unicast::Lane;
pub use output::PingPeer;
// Wire/runtime constants the external test + bench crates assert against; the
// rest of `util::consts` stays engine-internal.
pub use agent_habilis_mesh::util::consts::{
    MAX_LOGICAL_BODY_BYTES, MAX_MESSAGE_SIZE, MAX_SHARD_TOTAL, MESH_GLYPH,
};
pub use agent_habilis_mesh::util::version::VERSION;
pub use agent_habilis_mesh::util::{ensure_runtime_base, mesh_prefix, runtime_base};
pub use output::{OutputEvent, event_json, surfaced_event_json};

use anyhow::Result;

use cli::Cli;

/// Parse `argv` and run the selected CLI subcommand to completion.
///
/// This is the entire body of the `agent-square` binary; it is
/// public so the thin `src/main.rs` shim (which owns only
/// process-level concerns: tracing init, terminal echo) can call it.
/// The subcommand dispatch + per-command logic lives in `cli`.
///
/// # Errors
/// Propagates any error from the selected subcommand — mesh setup
/// failure, join timeout, IPC errors, invalid mesh-mode flags, etc.
pub async fn run_cli() -> Result<()> {
    // Parse through `cli_command()` (not `Cli::parse()`) so the `help`
    // subcommand blurb override in that builder applies at runtime too.
    let matches = cli_command().get_matches();
    let cli = <Cli as clap::FromArgMatches>::from_arg_matches(&matches)
        .unwrap_or_else(|error| error.exit());
    // Box the single large await so it lives on the heap, not this frame —
    // the same lever the event loop uses (see `daemon::event_loop`); the
    // setup → run future is near clippy's `large_futures` budget.
    Box::pin(cli::dispatch(cli)).await
}

/// The fully-built `agent-square` clap command tree, for offline man-page
/// generation (`cargo task man` walks it in-process through
/// `clap_mangen`). Arg surface only; no iroh, no runtime state.
#[must_use]
pub fn cli_command() -> clap::Command {
    // Override clap's auto-generated `help` subcommand blurb — its default
    // ("… of the given subcommand(s)") is the one `(s)` plural we can't reach
    // from a `///` doc, since clap owns the string. The subcommand is only
    // materialized once the command is built, so `build()` first, then
    // `mut_subcommand` (a later `get_matches`/render is a no-op rebuild that
    // preserves this). Keeps `agent-square help <cmd>` behavior intact.
    let mut command = <Cli as clap::CommandFactory>::command();
    command.build();
    command.mut_subcommand("help", |help| {
        help.about("Print this message or the help of the given subcommand")
    })
}

/// Build the deferred log sink and register it process-globally.
/// Call once in `main` before subscriber init; pass the returned
/// value to `tracing_subscriber::fmt().with_writer(..)`. Logs buffer
/// until `cli` resolves the mesh id + nickname (see `logging`).
#[must_use]
pub fn install_log_sink() -> LogSink {
    agent_habilis_mesh::logging::install()
}

/// The default tracing directive filter; pass to
/// `tracing_subscriber::fmt().with_env_filter(..)`. `RUST_LOG` overrides
/// it. See `logging`.
#[must_use]
pub fn log_filter() -> tracing_subscriber::EnvFilter {
    agent_habilis_mesh::logging::log_filter()
}

/// Flush buffered logs to stderr if identity was never resolved
/// (transient command, or startup failed before attach). Call after
/// `run_cli` returns.
pub fn flush_log_if_pending() {
    agent_habilis_mesh::logging::flush_pending_to_stderr();
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
