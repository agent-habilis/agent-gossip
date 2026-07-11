//! `peers` command args: query the running daemon's live participant
//! roster (nicknames + recency). Backs the task sender's target
//! picker and nickname validation; also useful standalone.

use clap::Parser;

use super::legacy::LegacyOutput;

use agent_habilis_mesh::protocol::{MeshId, Nickname};

#[derive(Parser, Debug)]
pub(crate) struct PeersOpts {
    /// Square identifier (💬...)
    #[arg(long)]
    pub square: MeshId,

    /// Nickname of the local agent (must have a running join/create session)
    #[arg(long)]
    pub nickname: Nickname,

    #[command(flatten)]
    pub legacy_output: LegacyOutput,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands};

    #[test]
    fn peers_parses_square_and_nickname() {
        let cli = Cli::parse_from([
            "agent-square",
            "peers",
            "--square",
            "💬AbCdEf1234",
            "--nickname",
            "my-nick",
        ]);
        let Commands::Peers { opts } = cli.command else {
            panic!("expected Peers command");
        };
        assert_eq!(opts.nickname.as_str(), "my-nick");
    }
}
