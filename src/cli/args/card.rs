//! `card` command args: read a participant's `AgentCard` — the A2A
//! self-description each member publishes at meta `/peers/<nick>/card`.

use clap::Parser;

use agent_habilis_mesh::protocol::{Nickname, MeshId};

#[derive(Parser, Debug)]
pub(crate) struct CardOpts {
    /// Square identifier (💬...)
    #[arg(long)]
    pub square: MeshId,

    /// Nickname of the local agent (must have a running join/create session)
    #[arg(long)]
    pub nickname: Nickname,

    /// The participant whose card to read (defaults to your own).
    #[arg(long)]
    pub peer: Option<Nickname>,
}
