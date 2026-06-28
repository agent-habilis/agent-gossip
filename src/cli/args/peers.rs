//! `peers` command args: query the running daemon's live participant
//! roster (nicknames + recency). Backs the handover sender's target
//! picker and nickname validation; also useful standalone.

use clap::Parser;

use crate::protocol::{Nickname, SwarmId};

use super::output::OutputFormat;

#[derive(Parser, Debug)]
pub(crate) struct PeersOpts {
    /// Swarm identifier (🐝...)
    #[arg(long)]
    pub swarm: SwarmId,

    /// Nickname of the local agent (must have a running join/create session)
    #[arg(long)]
    pub nickname: Nickname,

    /// Output format. Accepted for parity with other commands; `peers`
    /// always emits structured JSON.
    #[arg(long, default_value = "human")]
    pub output: OutputFormat,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands, OutputFormat};

    #[test]
    fn peers_accepts_output_flag() {
        let cli = Cli::parse_from([
            "ahsw", "peers", "--swarm", "🐝AbCdEf1234", "--nickname", "my-nick", "--output", "json",
        ]);
        let Commands::Peers { opts } = cli.command else {
            panic!("expected Peers command");
        };
        assert_eq!(opts.output, OutputFormat::Json);
    }

    #[test]
    fn peers_output_defaults_to_human() {
        let cli =
            Cli::parse_from(["ahsw", "peers", "--swarm", "🐝AbCdEf1234", "--nickname", "my-nick"]);
        let Commands::Peers { opts } = cli.command else {
            panic!("expected Peers command");
        };
        assert_eq!(opts.output, OutputFormat::Human);
    }
}
