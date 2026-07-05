//! `ping` command args: arm an RTT round on the running daemon.
//! Fire-and-forget — the `ping_report` arrives on the daemon's
//! `--output json` stream, not on this command's stdout.

use clap::Parser;

use agent_habilis_gossip::protocol::{Nickname, SwarmId};

use super::output::OutputFormat;

#[derive(Parser, Debug)]
pub(crate) struct PingOpts {
    /// Swarm identifier (💬...)
    #[arg(long)]
    pub swarm: SwarmId,

    /// Nickname of the local agent (must have a running join/create session)
    #[arg(long)]
    pub nickname: Nickname,

    /// Output format. Accepted for parity with other commands; the
    /// `ping_report` arrives on the daemon's own `--output json` stream.
    #[arg(long, default_value = "human")]
    pub output: OutputFormat,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands, OutputFormat};

    #[test]
    fn ping_accepts_output_flag() {
        let cli = Cli::parse_from([
            "agent-gossip",
            "ping",
            "--swarm",
            "💬AbCdEf1234",
            "--nickname",
            "my-nick",
            "--output",
            "json",
        ]);
        let Commands::Ping { opts } = cli.command else {
            panic!("expected Ping command");
        };
        assert_eq!(opts.output, OutputFormat::Json);
    }
}
