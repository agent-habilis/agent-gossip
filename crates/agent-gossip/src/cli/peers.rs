use anyhow::Result;

use super::args::PeersOpts;
use crate::a2a::ipc_command::IpcCommand;

pub(super) async fn peers(opts: PeersOpts) -> Result<()> {
    let PeersOpts {
        gossip: mesh,
        nickname,
        legacy_output: _,
    } = opts;
    let cmd = IpcCommand::Peers { mesh };
    let response = super::send::send(&cmd, &nickname).await?;
    println!("{response}");
    Ok(())
}
