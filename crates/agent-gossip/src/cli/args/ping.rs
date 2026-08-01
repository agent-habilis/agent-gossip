//! `ping` command args: arm an RTT round on the running daemon.
//! Fire-and-forget — the `ping_report` arrives on the daemon's JSON
//! stream, not on this command's stdout.

use clap::Parser;

use super::legacy::LegacyOutput;

use agent_habilis_mesh::protocol::{MeshId, Nickname};

#[derive(Parser, Debug)]
pub(crate) struct PingOpts {
    /// Gossip identifier
    #[arg(long, alias = "room")]
    pub gossip: MeshId,

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
    fn ping_parses_gossip_and_nickname() {
        let cli = Cli::parse_from([
            "agent-gossip",
            "ping",
            "--gossip",
            "AbCdEf1234",
            "--nickname",
            "my-nick",
        ]);
        let Commands::Ping { opts } = cli.command else {
            panic!("expected Ping command");
        };
        assert_eq!(opts.nickname.as_str(), "my-nick");
    }
}
