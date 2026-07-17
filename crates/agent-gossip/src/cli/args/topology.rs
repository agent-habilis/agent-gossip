//! `topology` command args: print the multihop routing topology (the assembled
//! room graph) from a running daemon's point of view, as JSON.

use clap::Parser;

use agent_habilis_mesh::protocol::{MeshId, Nickname};

#[derive(Parser, Debug)]
pub(crate) struct TopologyOpts {
    /// Room identifier (💬...)
    #[arg(long)]
    pub room: MeshId,

    /// Nickname of the local agent (must have a running join/create session)
    #[arg(long)]
    pub nickname: Nickname,
}
