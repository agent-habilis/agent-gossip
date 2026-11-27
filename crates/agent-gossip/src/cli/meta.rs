use anyhow::Result;

use super::args::MetaAction;
use super::parse_merge::parse_merge;
use super::reply;
use crate::a2a::ipc_command::IpcCommand;

/// Read or change the mesh's `meta` channel via the running daemon — the
/// independent counterpart of `state`. Same raw-JSON + exit-code contract.
pub(super) async fn meta(action: MetaAction) -> Result<()> {
    let (command, nickname) = match action {
        MetaAction::Merge { session, merge } => (
            IpcCommand::MetaMerge {
                mesh: session.gossip,
                merge: parse_merge(&merge)?,
            },
            session.nickname,
        ),
        MetaAction::Get { session } => (
            IpcCommand::MetaGet {
                mesh: session.gossip,
            },
            session.nickname,
        ),
    };
    reply::send_printed(&command, &nickname).await
}
