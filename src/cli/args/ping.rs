//! `ping` command args: arm an RTT round on the running daemon.
//! Fire-and-forget — the `ping_report` arrives on the daemon's
//! `--output json` stream, not on this command's stdout.

use clap::Parser;

use crate::protocol::{Nickname, SwarmId};

#[derive(Parser, Debug)]
pub(crate) struct PingOpts {
    /// Swarm identifier (🐝...)
    #[arg(long)]
    pub swarm: SwarmId,

    /// Nickname of the local agent (must have a running join/create session)
    #[arg(long)]
    pub nickname: Nickname,
}
