use anyhow::Result;

use super::args::InviteOpts;
use super::parse_ttl::parse_ttl;
use crate::a2a::ipc_command::IpcCommand;

/// Mint an invite via the creating session's daemon (only it holds the issuer
/// key). Prints the raw `{ok,invite}` line. Exits non-zero on refusal (e.g.
/// not the creator).
pub(super) async fn invite(opts: InviteOpts) -> Result<()> {
    let InviteOpts {
        gossip: mesh,
        nickname,
        ttl,
        legacy_output: _,
    } = opts;
    let ttl = parse_ttl(ttl.as_deref())?;
    let cmd = IpcCommand::Invite { mesh, ttl };
    let resp = super::send::send(&cmd, &nickname).await?;
    println!("{resp}");
    let parsed: serde_json::Value = serde_json::from_str(&resp)?;
    if parsed.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        std::process::exit(1);
    }
    Ok(())
}
