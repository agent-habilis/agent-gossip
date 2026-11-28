//! `topology` command args: print the multihop routing topology (the assembled
//! gossip graph) from a running daemon's point of view, as JSON.

use clap::Parser;

use super::session_address::SessionAddress;

#[derive(Parser, Debug)]
pub(crate) struct TopologyOpts {
    #[command(flatten)]
    pub session: SessionAddress,

    /// Draw the mesh as text (one box per peer, its path, the links between
    /// the other peers) instead of printing the raw JSON
    #[arg(long)]
    pub draw: bool,
}
