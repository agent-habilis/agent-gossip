use clap::Args;
use habilis_network::protocol::{MeshId, Nickname};

#[derive(Args, Debug)]
pub(crate) struct SessionAddress {
    /// Gossip identifier
    #[arg(long, alias = "room")]
    pub gossip: MeshId,

    /// Nickname of the local agent (must have a running join/create session)
    #[arg(long)]
    pub nickname: Nickname,
}
