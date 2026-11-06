use rmcp::model::ErrorData as McpError;

use crate::a2a::state_refusal_reply::StateRefusalReply;
use crate::api::SessionError;
use crate::mcp::to_mcp_error;

/// The MCP error of a state write. A refusal of the shared state (not synced, full, too deep, too
/// large) is an invalid request that carries its `reason` and the way out in `data`: the agent
/// can act on it. Any other failure is internal.
pub(crate) fn state_merge_error(error: SessionError) -> McpError {
    let reply = match &error {
        SessionError::Loop(cause) => StateRefusalReply::of(cause),
        _ => None,
    };
    match reply {
        Some(reply) => McpError::invalid_params(reply.text.clone(), Some(reply.to_data())),
        None => to_mcp_error(error),
    }
}
