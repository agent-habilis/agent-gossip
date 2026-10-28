use anyhow::Result;

use super::args::TopologyOpts;
use super::msg_response::MsgResponse;
use crate::a2a::ipc::IpcCommand;

/// Print the multihop routing topology (assembled mesh graph) from the running
/// daemon, as JSON.
pub(super) async fn topology_cmd(opts: TopologyOpts) -> Result<()> {
    let cmd = IpcCommand::Topology { mesh: opts.gossip };
    let resp = super::send::send(&cmd, &opts.nickname).await?;
    println!("{resp}");
    let parsed: MsgResponse = serde_json::from_str(&resp)?;
    if !parsed.ok {
        std::process::exit(1);
    }
    Ok(())
}
