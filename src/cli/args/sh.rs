use clap::Subcommand;

use super::output::OutputFormat;
use crate::protocol::SwarmId;

/// The `ahsw sh` actions — broadcast a live terminal, or attach to one.
#[derive(Subcommand, Debug)]
pub(crate) enum ShAction {
    /// Broadcast a live shell to peers; prints the `ahsw sh connect 🐝…`
    /// command on stdout.
    ///
    /// Spawns `$SHELL` in a pseudo-terminal that you use normally; its output is
    /// mirrored to every viewer. Viewers holding the printed ticket watch
    /// read-only — their keyboards never reach your shell. `--write` also mints
    /// a second, write-capable ticket whose holders type into your shell.
    /// Ending the shell (`exit` / Ctrl-D) ends the broadcast.
    Listen {
        /// Swarm id whose discovery config (local / mDNS / DHT / relay) the
        /// session should use, so it traverses the network like swarm members
        /// do. Omit for a public default.
        #[arg(long)]
        swarm: Option<SwarmId>,
        /// Output format: human (default) — a cargo-style status + hint — or json,
        /// the direct `ahsw sh connect 🐝…` line(s) for machines (read ticket
        /// first; the write ticket, if any, on a second line).
        #[arg(long, default_value = "human")]
        output: OutputFormat,
        /// Also mint a second, write-capable ticket. Anyone holding it can type
        /// into your shell (all writers' keys interleave) — share with care.
        #[arg(long)]
        write: bool,
        /// Run this command via `sh -c` instead of `$SHELL`. Hidden test/ops knob.
        #[arg(long, hide = true)]
        command: Option<String>,
        /// Force the shared terminal width instead of querying the tty. Hidden
        /// test knob (paired with `--rows`).
        #[arg(long, hide = true)]
        cols: Option<u16>,
        /// Force the shared terminal height instead of querying the tty. Hidden
        /// test knob (paired with `--cols`).
        #[arg(long, hide = true)]
        rows: Option<u16>,
    },
    /// Redeem a ticket and render the peer's terminal.
    ///
    /// The source's bytes are written verbatim; if the source is larger than your
    /// terminal the overflow is not shown. A read ticket is view-only — press
    /// Ctrl-C or `q` to detach. A write ticket forwards your keyboard to the
    /// shell; detach with `Enter ~ .` (type `~~` at a line start for a literal
    /// `~`).
    Connect {
        /// The `🐝…` ticket printed by `ahsw sh listen`.
        ticket: String,
    },
}
