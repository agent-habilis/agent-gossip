//! `peers` command args: query the running daemon's live peer
//! roster (nicknames + recency). Backs the task sender's target
//! picker and nickname validation; also useful standalone.

use clap::Parser;

use super::legacy_output::LegacyOutput;
use super::session_address::SessionAddress;

#[derive(Parser, Debug)]
pub(crate) struct PeersOpts {
    #[command(flatten)]
    pub session: SessionAddress,

    #[command(flatten)]
    pub legacy_output: LegacyOutput,
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

    #[test]
    fn legacy_room_alias_still_parses() {
        // Stale installed skills from the previous release pass `--room`;
        // the hidden alias keeps them from exit-2ing inside their
        // `>/dev/null 2>&1` launch lines. Pins one alias for all thirteen —
        // they are the same attribute on the same renamed field.
        let via_alias = Cli::parse_from([
            "agent-gossip",
            "peers",
            "--room",
            "AbCdEf1234",
            "--nickname",
            "my-nick",
        ]);
        let via_flag = Cli::parse_from([
            "agent-gossip",
            "peers",
            "--gossip",
            "AbCdEf1234",
            "--nickname",
            "my-nick",
        ]);
        let (Commands::Peers { options }, Commands::Peers { options: expected }) =
            (via_alias.command, via_flag.command)
        else {
            panic!("expected Peers commands");
        };
        assert_eq!(
            options.session.gossip.as_str(),
            expected.session.gossip.as_str(),
            "the gossip id parses to the value given on the command line"
        );
    }
}
