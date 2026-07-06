//! `topology` command args: print the circuit routing topology (the assembled
//! square graph) from a running daemon's point of view, as JSON.

use clap::Parser;

use agent_habilis_mesh::protocol::{MeshId, Nickname};

#[derive(Parser, Debug)]
pub(crate) struct TopologyOpts {
    /// Square identifier (💬...)
    #[arg(long)]
    pub square: MeshId,

    /// Nickname of the local agent (must have a running join/create session)
    #[arg(long)]
    pub nickname: Nickname,
}
