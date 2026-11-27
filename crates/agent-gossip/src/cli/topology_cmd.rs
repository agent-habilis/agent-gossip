use anyhow::Result;

use super::args::TopologyOpts;
use super::reply;
use crate::a2a::ipc_command::IpcCommand;
use crate::topology_doc::{TopologyDoc, render, warnings};

/// Print the multihop routing topology (assembled mesh graph) from the running
/// daemon: as JSON, or drawn as text with `--draw`.
pub(super) async fn topology_cmd(options: TopologyOpts) -> Result<()> {
    let command = IpcCommand::Topology {
        mesh: options.session.gossip.clone(),
    };
    let response = reply::send_accepted(&command, &options.session.nickname).await?;
    if !options.draw {
        println!("{response}");
        return Ok(());
    }
    let peers_command = IpcCommand::Peers {
        mesh: options.session.gossip,
    };
    let peers_response = reply::send_accepted(&peers_command, &options.session.nickname).await?;
    let doc = TopologyDoc::assemble(
        options.session.nickname.as_str(),
        &serde_json::from_str(&peers_response)?,
        &serde_json::from_str(&response)?,
    );
    print!("{}", render(&doc));
    for line in warnings(&doc) {
        eprintln!("{line}");
    }
    Ok(())
}
