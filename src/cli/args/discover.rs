//! `discover` command args: browse swarms advertising in a directory.
//! (The live picker runtime is [`crate::cli::discover`].)

use clap::Parser;

use crate::protocol::swarm::SwarmName;

use super::shared::SharedServerOpts;

#[derive(Parser, Debug)]
pub(crate) struct DiscoverOpts {
    /// The directory to browse. Omit for the well-known `global`
    /// directory. Must match the directory publishers passed to
    /// `--advertise`.
    #[arg(long)]
    pub directory: Option<SwarmName>,

    #[command(flatten)]
    pub shared: SharedServerOpts,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands};
    use crate::protocol::swarm::SwarmName;

    #[test]
    fn discover_parses_directory() {
        fn directory_of(args: &[&str]) -> Option<SwarmName> {
            match Cli::parse_from(args).command {
                Commands::Discover { opts } => opts.directory,
                Commands::Create { .. }
                | Commands::Join { .. }
                | Commands::Msg { .. }
                | Commands::Poll { .. }
                | Commands::Ping { .. }
                | Commands::Mcp => panic!("expected Discover"),
            }
        }
        // Bare discover ⇒ no explicit directory (defaults to global downstream).
        assert!(directory_of(&["ahs", "discover"]).is_none());
        // `--directory` is decoded into a SwarmName.
        assert_eq!(
            directory_of(&["ahs", "discover", "--directory", "gamedev"])
                .as_ref()
                .map(SwarmName::as_str),
            Some("gamedev")
        );
    }
}
