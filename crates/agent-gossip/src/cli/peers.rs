use anyhow::Result;

use super::args::PeersOpts;
use crate::a2a::ipc_command::IpcCommand;

/// Query the running daemon's live peer roster. Always emits the
/// raw IPC JSON (`{ok, peers, peer_count}`), like `poll`.
pub(super) async fn peers(options: PeersOpts) -> Result<()> {
    let PeersOpts { session } = options;
    let command = IpcCommand::Peers {
        mesh: session.gossip,
    };
    super::reply::send_printed(&command, &session.nickname).await
}
