use rmcp::model::ErrorData as McpError;

use super::session::Session;

pub(crate) fn already_in_mesh_error(existing: &Session) -> McpError {
    McpError::invalid_request(
        format!(
            "already in gossip {} as {}; call leave_gossip first",
            existing.mesh(),
            existing.nickname()
        ),
        None,
    )
}
