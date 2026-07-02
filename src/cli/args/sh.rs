use clap::Subcommand;

use super::lookup::PublicLookupArgs;
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
        /// do. Omit for a public default. Alternative to the
        /// `--mdns`/`--dht`/`--relay` flags — pass one or the other.
        #[arg(long, conflicts_with_all = ["public", "mdns", "dht", "relay"])]
        swarm: Option<SwarmId>,
        /// Which lookup mechanisms the session uses (same flags as `create`):
        /// naming any uses only those; naming none (or `--public`) is the
        /// all-on public preset.
        #[command(flatten)]
        lookups: PublicLookupArgs,
        /// Output format: human (default) — a cargo-style status + hint — or json,
        /// the direct `ahsw sh connect 🐝…` line(s) for machines (read ticket
        /// first; the write ticket, if any, on a second line).
        #[arg(long, default_value = "human")]
        output: OutputFormat,
        /// Also mint a second, write-capable ticket. Anyone holding it can type
        /// into your shell (all writers' keys interleave) — share with care.
        #[arg(long)]
        write: bool,
        /// Protect the shell with a password: the ticket alone no longer
        /// admits — viewers must present the password (so a passworded ticket
        /// is safe to share). One password guards both the read and write
        /// tickets. Bare `--password` prompts hidden on the terminal;
        /// `--password=<pw>` passes it inline (visible in `ps` — prefer the
        /// prompt when a human types it).
        #[arg(long, num_args(0..=1), require_equals = true)]
        #[expect(
            clippy::option_option,
            reason = "clap optional-value flag: absent/bare/valued are three distinct password states"
        )]
        password: Option<Option<String>>,
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
        /// Password for a password-protected ticket — required exactly when
        /// the ticket carries the password flag (as a prompt on a terminal,
        /// or inline via `--password=<pw>`).
        #[arg(long, num_args(0..=1), require_equals = true)]
        #[expect(
            clippy::option_option,
            reason = "clap optional-value flag: absent/bare/valued are three distinct password states"
        )]
        password: Option<Option<String>>,
    },
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands};

    #[test]
    fn sh_listen_parses_lookup_flags() {
        let cli = Cli::parse_from(["ahsw", "sh", "listen", "--mdns"]);
        let Commands::Sh { action } = cli.command else {
            panic!("expected Sh");
        };
        let super::ShAction::Listen { swarm, lookups, .. } = action else {
            panic!("expected Listen");
        };
        assert!(swarm.is_none());
        assert!(!lookups.public);
        assert!(lookups.lookups.mdns && !lookups.lookups.dht);
    }

    #[test]
    fn sh_listen_parses_public() {
        let cli = Cli::parse_from(["ahsw", "sh", "listen", "--public"]);
        let Commands::Sh { action } = cli.command else {
            panic!("expected Sh");
        };
        let super::ShAction::Listen { lookups, .. } = action else {
            panic!("expected Listen");
        };
        assert!(lookups.public);
    }
}
