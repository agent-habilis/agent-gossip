use crate::cli;
use crate::cli::Cli;
use crate::cli_command::cli_command;
use crate::cli_error::CliError;
use crate::register_build_version::register_build_version;

/// Parse `argv` and run the selected CLI subcommand to completion.
///
/// This is the entire body of the `agent-gossip` binary; it is
/// public so the thin `src/main.rs` shim (which owns only
/// process-level concerns: tracing init, terminal echo) can call it.
/// The subcommand dispatch + per-command logic lives in `cli`.
///
/// # Errors
/// Propagates any error from the selected subcommand — mesh setup
/// failure, join timeout, IPC errors, invalid mesh-mode flags, etc.
pub async fn run_cli() -> Result<(), CliError> {
    register_build_version();
    // Parse through `cli_command()` (not `Cli::parse()`) so the `help`
    // subcommand blurb override in that builder applies at runtime too.
    let matches = cli_command().get_matches();
    let cli = <Cli as clap::FromArgMatches>::from_arg_matches(&matches)
        .unwrap_or_else(|error| error.exit());
    // Box the single large await so it lives on the heap, not this frame —
    // the same lever the event loop uses (see `daemon::event_loop`); the
    // setup → run future is near clippy's `large_futures` budget.
    Box::pin(cli::dispatch(cli))
        .await
        .map_err(CliError::Command)
}
