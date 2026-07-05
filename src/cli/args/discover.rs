//! `discover` command args: browse swarms advertising in a directory.
//! (The live picker runtime is [`crate::cli::discover`].)

use clap::Parser;

use agent_habilis_gossip::protocol::swarm::SwarmName;

use super::lookup::PublicLookupArgs;
use super::shared::SharedServerOpts;

#[derive(Parser, Debug)]
pub(crate) struct DiscoverOpts {
    /// The directory to browse. Omit for the well-known `global`
    /// directory. Must match the directory publishers passed to
    /// `--advertise`.
    #[arg(long)]
    pub directory: Option<SwarmName>,

    /// Lookups used to reach the directory (`--mdns`/`--dht`/`--relay`).
    /// Naming none (or `--public`) uses all three; naming any restricts to
    /// those (a disabled leg makes no network requests). Must match the
    /// lookups the advertiser used — an mDNS-only advertiser is found only
    /// by an mDNS-only `discover`.
    #[command(flatten)]
    pub lookups: PublicLookupArgs,

    #[command(flatten)]
    pub shared: SharedServerOpts,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands};
    use agent_habilis_gossip::protocol::swarm::SwarmName;

    #[test]
    fn discover_parses_directory() {
        fn directory_of(args: &[&str]) -> Option<SwarmName> {
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
                | Commands::Session { .. } => panic!("expected Discover"),
            }
        }
        // Bare discover ⇒ no explicit directory (defaults to global downstream).
        assert!(directory_of(&["agent-gossip", "discover"]).is_none());
        // `--directory` is decoded into a SwarmName.
        assert_eq!(
            directory_of(&["agent-gossip", "discover", "--directory", "gamedev"])
                .as_ref()
                .map(SwarmName::as_str),
            Some("gamedev")
        );
    }
}
