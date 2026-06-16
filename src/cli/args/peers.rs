//! `peers` command args: query the running daemon's live participant
//! roster (nicknames + recency). Backs the handover sender's target
//! picker and nickname validation; also useful standalone.

use clap::Parser;

use crate::protocol::{Nickname, SwarmId};

#[derive(Parser, Debug)]
pub(crate) struct PeersOpts {
    /// Swarm identifier (ahs...)
    #[arg(long)]
    pub swarm: SwarmId,

    /// Nickname of the local agent (must have a running join/create session)
    #[arg(long)]
    pub nickname: Nickname,
}
