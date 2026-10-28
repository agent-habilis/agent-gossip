use rmcp::model::ErrorData as McpError;

pub(crate) fn to_mcp_error<E: std::fmt::Display>(error: E) -> McpError {
    McpError::internal_error(error.to_string(), None)
}
