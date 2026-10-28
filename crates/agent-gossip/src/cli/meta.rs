use anyhow::Result;

use super::args::{MetaAction, MetaOpts};
use super::msg_response::MsgResponse;
use crate::a2a::ipc_command::IpcCommand;

/// Read or change the mesh's `meta` channel via the running daemon — the
/// independent counterpart of [`state`]. Same raw-JSON + exit-code contract.
pub(super) async fn meta(options: MetaOpts) -> Result<()> {
    let (cmd, nickname) = match options.action {
        MetaAction::Merge {
            gossip: mesh,
            nickname,
            merge,
        } => {
            let merge_doc: serde_json::Value = serde_json::from_str(&merge).map_err(|error| {
                anyhow::anyhow!("--merge must be valid JSON (an RFC 7386 merge patch): {error}")
            })?;
            (
                IpcCommand::MetaMerge {
                    mesh,
                    merge: merge_doc,
                },
                nickname,
            )
        }
        MetaAction::Get {
            gossip: mesh,
            nickname,
        } => (IpcCommand::MetaGet { mesh }, nickname),
    };
    let response = super::send::send(&cmd, &nickname).await?;
    println!("{response}");
    let parsed: MsgResponse = serde_json::from_str(&response)?;
    if !parsed.ok {
        std::process::exit(1);
    }
    Ok(())
}
