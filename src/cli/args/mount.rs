use std::path::PathBuf;

use clap::Subcommand;

use super::output::OutputFormat;
use crate::protocol::SwarmId;

/// The `ahsw mount serve` action. The consumer side is the bare
/// `ahsw mount <🐝…> <mountpoint>` form (positionals on the parent command),
/// so a `🐝…` ticket can never collide with the `serve` literal.
#[derive(Subcommand, Debug)]
pub(crate) enum MountAction {
    /// Share a folder read-only; prints the `ahsw mount 🐝…` command on stdout.
    ///
    /// Lazy: the tree is scanned once at startup (a metadata-only snapshot —
    /// nothing is hashed or transferred up front); peers fetch file bytes on
    /// demand as they read them. Changes after startup are not visible until
    /// you restart `serve` and peers remount. Keeps serving until interrupted.
    Serve {
        /// The directory to share.
        dir: PathBuf,
        /// Swarm id whose discovery config (local / mDNS / DHT / relay) the
        /// share should use, so it traverses the network like swarm members
        /// do. Omit for a public default.
        #[arg(long)]
        swarm: Option<SwarmId>,
        /// Output format: human (default) — a cargo-style status + hint — or
        /// json, a single direct `ahsw mount 🐝…` line for machines.
        #[arg(long, default_value = "human")]
        output: OutputFormat,
    },
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands};

    #[test]
    fn mount_serve_parses() {
        let cli = Cli::parse_from(["ahsw", "mount", "serve", "./dir"]);
        let Commands::Mount { action, .. } = cli.command else {
            panic!("expected Mount");
        };
        let Some(super::MountAction::Serve { dir, swarm, .. }) = action else {
            panic!("expected Serve");
        };
        assert_eq!(dir, std::path::PathBuf::from("./dir"));
        assert!(swarm.is_none());
    }

    #[test]
    fn mount_ticket_form_parses() {
        let cli = Cli::parse_from(["ahsw", "mount", "🐝abc", "./mnt", "--output", "json"]);
        let Commands::Mount {
            action,
            ticket,
            mountpoint,
            no_mount,
            output,
        } = cli.command
        else {
            panic!("expected Mount");
        };
        assert!(action.is_none());
        assert_eq!(ticket.as_deref(), Some("🐝abc"));
        assert_eq!(mountpoint, Some(std::path::PathBuf::from("./mnt")));
        assert!(!no_mount);
        assert!(matches!(output, super::OutputFormat::Json));
    }

    #[test]
    fn mount_without_args_is_rejected_at_dispatch_not_parse() {
        // Both positionals are optional at the clap layer (the serve
        // subcommand shares the slot); the handler errors with usage.
        let cli = Cli::parse_from(["ahsw", "mount"]);
        let Commands::Mount { action, ticket, .. } = cli.command else {
            panic!("expected Mount");
        };
        assert!(action.is_none());
        assert!(ticket.is_none());
    }
}
