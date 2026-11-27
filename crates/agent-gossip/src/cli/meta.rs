use anyhow::Result;

use super::args::MetaAction;
use super::msg_response::MsgResponse;
use crate::a2a::ipc_command::IpcCommand;

/// Read or change the mesh's `meta` channel via the running daemon — the
/// independent counterpart of `state`. Same raw-JSON + exit-code contract.
pub(super) async fn meta(action: MetaAction) -> Result<()> {
    let (command, nickname) = match action {
        MetaAction::Merge { session, merge } => {
            let merge_doc: serde_json::Value = serde_json::from_str(&merge).map_err(|error| {
                anyhow::anyhow!("--merge must be valid JSON (an RFC 7386 merge patch): {error}")
            })?;
            (
                IpcCommand::MetaMerge {
                    mesh: session.gossip,
                    merge: merge_doc,
                },
                session.nickname,
            )
        }
        MetaAction::Get { session } => (
            IpcCommand::MetaGet {
                mesh: session.gossip,
            },
            session.nickname,
        ),
    };
    let response = super::send::send(&command, &nickname).await?;
    println!("{response}");
    let parsed: MsgResponse = serde_json::from_str(&response)?;
    if !parsed.ok {
        std::process::exit(1);
    }
    Ok(())
}
