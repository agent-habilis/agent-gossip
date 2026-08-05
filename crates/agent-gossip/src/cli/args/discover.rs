//! `discover` command args: browse gossips advertising in a directory.
//! (The JSON-streaming runtime is [`crate::cli::discover`].)

use clap::Parser;

use fofoca::protocol::MeshName;

use super::legacy::LegacyOutput;
use super::lookup::PublicLookupArgs;
use super::tuning::TuningOpts;

#[derive(Parser, Debug)]
pub(crate) struct DiscoverOpts {
    /// The directory to browse. Omit for the well-known `global`
    /// directory. Must match the directory publishers passed to
    /// `--advertise`.
    #[arg(long)]
    pub directory: Option<MeshName>,

    /// Exit on our own after listening this many seconds (bounded collection
    /// window; exit code 0 whether or not anything was found). Omit to stream
    /// until interrupted. The window starts once the directory session is up.
    /// Advertisers re-broadcast every 20s, so 25 sees every live listing.
    #[arg(long)]
    pub window_secs: Option<u64>,

    /// Lookups used to reach the directory (`--mdns`/`--dht`/`--relay`).
    /// Naming none (or `--public`) uses all three; naming any restricts to
    /// those (a disabled leg makes no network requests). Must match the
    /// lookups the advertiser used — an mDNS-only advertiser is found only
    /// by an mDNS-only `discover`.
    #[command(flatten)]
    pub lookups: PublicLookupArgs,

    /// `discover` joins the directory as a pure consumer: it writes no state
    /// file, runs no gossip session, and serves no binding, so it takes only
    /// the hidden tuning knobs — not the daemon flags of `SharedServerOpts`,
    /// which it would silently ignore.
    #[command(flatten)]
    pub tuning: TuningOpts,

    #[command(flatten)]
    pub legacy_output: LegacyOutput,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands};
    use fofoca::protocol::MeshName;

    fn discover_opts(args: &[&str]) -> super::DiscoverOpts {
        match Cli::parse_from(args).command {
            Commands::Discover { opts } => opts,
            Commands::Create { .. }
            | Commands::Join { .. }
            | Commands::Topic { .. }
            | Commands::Poll { .. }
            | Commands::Ping { .. }
            | Commands::Mcp { .. }
            | Commands::Man
            | Commands::Peers { .. }
            | Commands::State { .. }
            | Commands::Meta { .. }
            | Commands::Topology { .. }
            | Commands::A2a { .. }
            | Commands::Ready { .. }
            | Commands::Plug { .. }
            | Commands::Unplug { .. }
            | Commands::Doctor { .. }
            | Commands::Leave { .. }
            | Commands::Invite { .. }
            | Commands::Session { .. } => panic!("expected Discover"),
        }
    }

    #[test]
    fn discover_parses_directory() {
        // Bare discover ⇒ no explicit directory (defaults to global downstream).
        assert!(
            discover_opts(&["agent-gossip", "discover"])
                .directory
                .is_none()
        );
        // `--directory` is decoded into a MeshName.
        assert_eq!(
            discover_opts(&["agent-gossip", "discover", "--directory", "gamedev"])
                .directory
                .as_ref()
                .map(MeshName::as_str),
            Some("gamedev")
        );
    }

    #[test]
    fn discover_parses_window_secs() {
        // Omitted ⇒ stream until interrupted (today's behavior).
        assert!(
            discover_opts(&["agent-gossip", "discover"])
                .window_secs
                .is_none()
        );
        assert_eq!(
            discover_opts(&["agent-gossip", "discover", "--window-secs", "25"]).window_secs,
            Some(25)
        );
    }
}
