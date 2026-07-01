use clap::Subcommand;

use super::output::OutputFormat;
use crate::protocol::SwarmId;

/// The `ahsw sh` actions — broadcast a live terminal, or watch one read-only.
#[derive(Subcommand, Debug)]
pub(crate) enum ShAction {
    /// Broadcast a live shell to peers; prints the `ahsw sh connect 🐝…`
    /// command on stdout.
    ///
    /// Spawns `$SHELL` in a pseudo-terminal that you use normally; its output is
    /// mirrored to every viewer read-only (a viewer's keyboard never reaches your
    /// shell). Ending the shell (`exit` / Ctrl-D) ends the broadcast.
    Listen {
        /// Swarm id whose discovery config (local / mDNS / DHT / relay) the
        /// session should use, so it traverses the network like swarm members
        /// do. Omit for a public default.
        #[arg(long)]
        swarm: Option<SwarmId>,
        /// Output format: human (default) — a cargo-style status + hint — or json,
        /// a single direct `ahsw sh connect 🐝…` line for machines.
        #[arg(long, default_value = "human")]
        output: OutputFormat,
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
    /// Redeem a ticket and render the peer's terminal read-only.
    ///
    /// The source's bytes are written verbatim; if the source is larger than your
    /// terminal the overflow is not shown. Press Ctrl-C or `q` to detach.
    Connect {
        /// The `🐝…` ticket printed by `ahsw sh listen`.
        ticket: String,
    },
}
