//! `topology` command args: print the circuit routing topology (the assembled
//! mesh graph) from a running daemon's point of view, as JSON.

use clap::Parser;

use agent_habilis_mesh::protocol::{MeshId, Nickname};

#[derive(Parser, Debug)]
pub(crate) struct TopologyOpts {
    /// Mesh identifier (💬...)
    #[arg(long)]
    pub mesh: MeshId,

    /// Nickname of the local agent (must have a running join/create session)
    #[arg(long)]
    pub nickname: Nickname,
}
