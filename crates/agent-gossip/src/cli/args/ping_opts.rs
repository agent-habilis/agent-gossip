//! `ping` command args: arm an RTT round on the running daemon.
//! Fire-and-forget — the `ping_report` arrives on the daemon's JSON
//! stream, not on this command's stdout.

use clap::Parser;

use super::session_address::SessionAddress;

#[derive(Parser, Debug)]
pub(crate) struct PingOpts {
    #[command(flatten)]
    pub session: SessionAddress,
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
        let Commands::Ping { options } = cli.command else {
            panic!("expected Ping command");
        };
        assert_eq!(
            options.session.nickname.as_str(),
            "my-nick",
            "--nickname sets the local nickname"
        );
    }
}
