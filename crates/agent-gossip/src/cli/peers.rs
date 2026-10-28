use anyhow::Result;

use super::args::PeersOpts;
use crate::a2a::ipc_command::IpcCommand;

pub(super) async fn peers(options: PeersOpts) -> Result<()> {
    let PeersOpts {
        gossip: mesh,
        nickname,
        legacy_output: _,
    } = options;
    let cmd = IpcCommand::Peers { mesh };
    let response = super::send::send(&cmd, &nickname).await?;
    println!("{response}");
    Ok(())
}
