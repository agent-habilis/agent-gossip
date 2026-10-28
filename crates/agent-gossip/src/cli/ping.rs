use anyhow::Result;

use super::args::PingOpts;
use super::msg_response::MsgResponse;
use crate::a2a::ipc_command::IpcCommand;

/// Arm an RTT round on the running daemon. Fire-and-forget: the daemon
/// acks immediately and emits the `ping_report` on its own
/// `--output json` stream once the collection window closes.
pub(super) async fn ping(options: PingOpts) -> Result<()> {
    let PingOpts {
        gossip: mesh,
        nickname,
        legacy_output: _,
    } = options;
    let cmd = IpcCommand::Ping { mesh };
    let response = super::send::send(&cmd, &nickname).await?;
    let parsed: MsgResponse = serde_json::from_str(&response)?;
    if !parsed.ok {
        anyhow::bail!(
            "{}",
            parsed.error.unwrap_or_else(|| "unknown error".to_string())
        );
    }
    Ok(())
}
