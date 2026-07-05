//! `topology` command args: print the relay routing topology (the assembled
//! mesh graph) from a running daemon's point of view, as JSON.

use clap::Parser;

use crate::protocol::{Nickname, SwarmId};

#[derive(Parser, Debug)]
pub(crate) struct TopologyOpts {
    /// Swarm identifier (💬...)
    #[arg(long)]
    pub swarm: SwarmId,

    /// Nickname of the local agent (must have a running join/create session)
    #[arg(long)]
    pub nickname: Nickname,
}
