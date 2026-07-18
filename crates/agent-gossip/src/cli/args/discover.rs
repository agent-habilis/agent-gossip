//! `discover` command args: browse gossips advertising in a directory.
//! (The JSON-streaming runtime is [`crate::cli::discover`].)

use clap::Parser;

use agent_habilis_mesh::protocol::mesh::MeshName;

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
    use agent_habilis_mesh::protocol::mesh::MeshName;

    #[test]
    fn discover_parses_directory() {
        fn directory_of(args: &[&str]) -> Option<MeshName> {
            match Cli::parse_from(args).command {
                Commands::Discover { opts } => opts.directory,
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
        // Bare discover ⇒ no explicit directory (defaults to global downstream).
        assert!(directory_of(&["agent-gossip", "discover"]).is_none());
        // `--directory` is decoded into a MeshName.
        assert_eq!(
            directory_of(&["agent-gossip", "discover", "--directory", "gamedev"])
                .as_ref()
                .map(MeshName::as_str),
            Some("gamedev")
        );
    }
}
