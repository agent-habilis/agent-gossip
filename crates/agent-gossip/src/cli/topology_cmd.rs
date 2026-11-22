use anyhow::Result;

use super::args::TopologyOpts;
use super::msg_response::MsgResponse;
use crate::a2a::ipc_command::IpcCommand;
use crate::topology_doc::{TopologyDoc, render, warnings};

/// Print the multihop routing topology (assembled mesh graph) from the running
/// daemon: as JSON, or drawn as text with `--draw`.
pub(super) async fn topology_cmd(options: TopologyOpts) -> Result<()> {
    let command = IpcCommand::Topology {
        mesh: options.gossip.clone(),
    };
    let response = super::send::send(&command, &options.nickname).await?;
    if !reply_is_ok(&response)? {
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
    if !reply_is_ok(&peers_response)? {
        println!("{peers_response}");
        std::process::exit(1);
    }
    let doc = TopologyDoc::assemble(
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

fn reply_is_ok(response: &str) -> Result<bool> {
    let parsed: MsgResponse = serde_json::from_str(response)?;
    Ok(parsed.ok)
}

#[cfg(test)]
mod tests {
    /// `topology --draw` exits 1 on a reply that is not ok, for the topology reply and for the
    /// peers reply alike. The exit call itself needs a daemon that answers one query and not the
    /// other, so the test pins the decision.
    #[test]
    fn a_reply_that_says_not_ok_fails_the_command() {
        let failed = r#"{"ok":false,"error":"no such gossip"}"#;
        let fine = r#"{"ok":true,"peers":[]}"#;

        assert!(
            !super::reply_is_ok(failed).expect("a reply is JSON"),
            "the daemon refused the query, so the command exits 1"
        );
        assert!(
            super::reply_is_ok(fine).expect("a reply is JSON"),
            "the daemon answered, so the command goes on to the drawing"
        );
    }
}
