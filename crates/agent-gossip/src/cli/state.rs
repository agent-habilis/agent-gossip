use anyhow::Result;
use habilis_network::protocol::Nickname;

use super::args::{MetaAction, MetaOpts, StateAction, StateOpts};
use super::msg_response::MsgResponse;
use crate::a2a::ipc_command::IpcCommand;

/// Read or change the mesh's shared state via the running daemon. Emits the
/// raw IPC JSON response — `{ok,...}` for `merge`, `{ok,document}` for `get`.
pub(super) async fn state(options: StateOpts) -> Result<()> {
    match options.action {
        StateAction::Merge {
            gossip: mesh,
            nickname,
            merge,
        } => {
            let merge = parse_merge(&merge)?;
            request(&IpcCommand::StateMerge { mesh, merge }, &nickname).await
        }
        StateAction::Get {
            gossip: mesh,
            nickname,
        } => request(&IpcCommand::StateGet { mesh }, &nickname).await,
    }
}

/// Read or change the mesh's `meta` channel via the running daemon — the
/// independent counterpart of `state`. Same raw-JSON + exit-code contract.
pub(super) async fn meta(options: MetaOpts) -> Result<()> {
    match options.action {
        MetaAction::Merge {
            gossip: mesh,
            nickname,
            merge,
        } => {
            let merge = parse_merge(&merge)?;
            request(&IpcCommand::MetaMerge { mesh, merge }, &nickname).await
        }
        MetaAction::Get {
            gossip: mesh,
            nickname,
        } => request(&IpcCommand::MetaGet { mesh }, &nickname).await,
    }
}

fn parse_merge(merge: &str) -> Result<serde_json::Value> {
    serde_json::from_str(merge).map_err(|error| {
        anyhow::anyhow!("--merge must be valid JSON (an RFC 7386 merge patch): {error}")
    })
}

async fn request(command: &IpcCommand, nickname: &Nickname) -> Result<()> {
    let response = super::send::send(command, nickname).await?;
    println!("{response}");
    // A rejected merge must not exit 0: a shell-driven agent
    // reads the exit code to tell an applied change from a rejected one, and an
    // `{"ok":false}` that exits 0 reads as success → silent desync. The raw JSON
    // is already printed above for `--output json` consumers; the exit code is
    // the scriptable signal. (`get` returns `ok:true`, so it stays exit 0.)
    let parsed: MsgResponse = serde_json::from_str(&response)?;
    if !parsed.ok {
        std::process::exit(1);
    }
    Ok(())
}
