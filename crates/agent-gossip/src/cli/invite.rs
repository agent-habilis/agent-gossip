use anyhow::Result;

use super::args::InviteOpts;
use super::parse_ttl::parse_ttl;
use super::reply::send_printed;
use crate::a2a::ipc_command::IpcCommand;

/// Mint an invite via the creating session's daemon (only it holds the issuer
/// key). Prints the raw `{ok,invite}` line. Exits non-zero on refusal (e.g.
/// not the creator).
pub(super) async fn invite(options: InviteOpts) -> Result<()> {
    let InviteOpts {
        gossip: mesh,
        nickname,
        ttl,
        legacy_output: _,
    } = options;
    let ttl = parse_ttl(ttl.as_deref())?;
    let command = IpcCommand::Invite { mesh, ttl };
    send_printed(&command, &nickname).await
}
