use crate::cli::Cli;

/// The fully-built `agent-gossip` clap command tree, for offline man-page
/// generation (`cargo task man` walks it in-process through
/// `clap_mangen`). Arg surface only; no iroh, no runtime state.
#[must_use]
pub fn cli_command() -> clap::Command {
    // Override clap's auto-generated `help` subcommand blurb — its default
    // ("… of the given subcommand(s)") is the one `(s)` plural we can't reach
    // from a `///` doc, since clap owns the string. The subcommand is only
    // materialized once the command is built, so `build()` first, then
    // `mut_subcommand` (a later `get_matches`/render is a no-op rebuild that
    // preserves this). Keeps `agent-gossip help <cmd>` behavior intact.
    let mut command = <Cli as clap::CommandFactory>::command();
    command.build();
    command.mut_subcommand("help", |help| {
        help.about("Print this message or the help of the given subcommand")
    })
}
