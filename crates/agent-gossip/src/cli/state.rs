use anyhow::Result;

use super::args::StateAction;
use super::parse_merge::parse_merge;
use super::reply::send_printed;
use crate::a2a::ipc_command::IpcCommand;

/// Read or change the mesh's shared state via the running daemon. Emits the
/// raw IPC JSON response — `{ok,...}` for `merge`, `{ok,document}` for `get`.
pub(super) async fn state(action: StateAction) -> Result<()> {
    let (command, nickname) = match action {
        StateAction::Merge { session, merge } => (
            IpcCommand::StateMerge {
                mesh: session.gossip,
                merge: parse_merge(&merge)?,
            },
            session.nickname,
        ),
        StateAction::Get { session } => (
            IpcCommand::StateGet {
                mesh: session.gossip,
            },
            session.nickname,
        ),
    };
    send_printed(&command, &nickname).await
}
