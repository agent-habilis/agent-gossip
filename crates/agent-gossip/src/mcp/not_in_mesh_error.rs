use rmcp::model::ErrorData as McpError;

pub(crate) fn not_in_mesh_error() -> McpError {
    McpError::invalid_request(
        "not in a gossip; call create_gossip or join_gossip first".to_string(),
        None,
    )
}
