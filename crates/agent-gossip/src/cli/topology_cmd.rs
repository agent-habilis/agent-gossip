use anyhow::Result;

use super::args::TopologyOpts;
use super::msg_response::MsgResponse;
use crate::a2a::ipc_command::IpcCommand;
use crate::topology_draw::{TopologyDoc, render, warnings};

/// Print the multihop routing topology (assembled mesh graph) from the running
/// daemon: as JSON, or drawn as text with `--draw`.
pub(super) async fn topology_cmd(options: TopologyOpts) -> Result<()> {
    let command = IpcCommand::Topology {
        mesh: options.gossip.clone(),
    };
    let response = super::send::send(&command, &options.nickname).await?;
    let parsed: MsgResponse = serde_json::from_str(&response)?;
    if !parsed.ok {
        println!("{response}");
        std::process::exit(1);
    }
    if !options.draw {
        println!("{response}");
        return Ok(());
    }
    let peers_command = IpcCommand::Peers {
        mesh: options.gossip,
    };
    let peers_response = super::send::send(&peers_command, &options.nickname).await?;
    let doc = TopologyDoc::from_responses(
        options.nickname.as_str(),
        &serde_json::from_str(&peers_response)?,
        &serde_json::from_str(&response)?,
    );
    print!("{}", render(&doc));
    for line in warnings(&doc) {
        eprintln!("{line}");
    }
    Ok(())
}
