use anyhow::Result;
use rmcp::model::{CallToolResult, ErrorData as McpError};

use super::session::Session;
use super::{MeshRef, already_in_mesh_error, ok_json};

/// Idempotency shared by `join_gossip` / `topic_gossip`: re-joining the gossip
/// the request resolves to, with the same (or no) nickname, is a no-op that
/// returns the existing session; any other request while in a mesh errors.
pub(crate) fn rejoin_existing(
    existing: &Session,
    requested_nickname: Option<&str>,
    same_mesh: bool,
) -> Result<CallToolResult, McpError> {
    let same_nickname =
        requested_nickname.is_none_or(|candidate| candidate == existing.nickname().as_str());
    if same_mesh && same_nickname {
        return ok_json(MeshRef::from(existing));
    }
    Err(already_in_mesh_error(existing))
}
