use anyhow::Result;

use super::args::PeersOpts;
use crate::a2a::ipc_command::IpcCommand;

/// Query the running daemon's live peer roster. Always emits the
/// raw IPC JSON (`{ok, peers, peer_count}`), like `poll`.
pub(super) async fn peers(options: PeersOpts) -> Result<()> {
    let PeersOpts {
        session,
        legacy_output: _,
    } = options;
    let command = IpcCommand::Peers {
        mesh: session.gossip,
    };
    let response = super::send::send(&command, &session.nickname).await?;
    println!("{response}");
    Ok(())
}
