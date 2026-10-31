use anyhow::Result;
use habilis_network::protocol::MessageId;

use super::msg_response::MsgResponse;

/// Reduce an IPC send response (`msg` / `task`, same `{ok,id,error}`
/// shape) to the new message id, or a descriptive error. `what` names the
/// operation for the missing-id message.
pub(super) fn finish_send(response: &str, what: &str) -> Result<MessageId> {
    let parsed: MsgResponse = serde_json::from_str(response)?;
    if !parsed.ok {
        anyhow::bail!(
            "{}",
            parsed.error.unwrap_or_else(|| "unknown error".to_string())
        );
    }
    parsed
        .id
        .ok_or_else(|| anyhow::anyhow!("{what} response missing id"))
}
