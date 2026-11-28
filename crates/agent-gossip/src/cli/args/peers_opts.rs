//! `peers` command args: query the running daemon's live peer
//! roster (nicknames + recency). Backs the task sender's target
//! picker and nickname validation; also useful standalone.

use clap::Parser;

use super::session_address::SessionAddress;

#[derive(Parser, Debug)]
pub(crate) struct PeersOpts {
    #[command(flatten)]
    pub session: SessionAddress,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands};

    #[test]
    fn peers_parses_gossip_and_nickname() {
        let cli = Cli::parse_from([
            "agent-gossip",
            "peers",
            "--gossip",
            "AbCdEf1234",
            "--nickname",
            "my-nick",
        ]);
        let Commands::Peers { options } = cli.command else {
            panic!("expected Peers command");
        };
        assert_eq!(
            options.session.nickname.as_str(),
            "my-nick",
            "--nickname sets the local nickname"
        );
    }
}
