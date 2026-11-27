use anyhow::Result;
use habilis_network::protocol::MessageId;

use super::reply;

/// Reduce an IPC send response (`msg` / `task`, same `{ok,id,error}`
/// shape) to the new message id, or a descriptive error. `what` names the
/// operation for the missing-id message.
pub(super) fn finish_send(response: &str, what: &str) -> Result<MessageId> {
    reply::accepted(response)?
        .id
        .ok_or_else(|| anyhow::anyhow!("{what} response missing id"))
}
