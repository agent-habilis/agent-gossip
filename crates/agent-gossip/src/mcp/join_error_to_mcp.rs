use rmcp::model::ErrorData as McpError;

use crate::api::JoinError;

/// A bad target/string is the caller's fault (`invalid_params`); a setup
/// failure is ours (`internal_error`).
pub(crate) fn join_error_to_mcp(error: JoinError) -> McpError {
    match error {
        JoinError::Resolve(error) => McpError::invalid_params(error.to_string(), None),
        JoinError::Setup(error) => McpError::internal_error(error.to_string(), None),
    }
}
