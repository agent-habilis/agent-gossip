use anyhow::Result;

use super::args::PingOpts;
use super::reply::accepted;
use crate::a2a::ipc_command::IpcCommand;

/// Arm an RTT round on the running daemon. Fire-and-forget: the daemon
/// acks immediately and emits the `ping_report` on its own
/// `--output json` stream once the collection window closes.
pub(super) async fn ping(options: PingOpts) -> Result<()> {
    let PingOpts {
        session,
        legacy_output: _,
    } = options;
    let command = IpcCommand::Ping {
        mesh: session.gossip,
    };
    let response = super::send::send(&command, &session.nickname).await?;
    accepted(&response)?;
    Ok(())
}
