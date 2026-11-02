use anyhow::Result;
use rmcp::model::{CallToolResult, Content, ErrorData as McpError};
use serde::Serialize;

use super::to_mcp_error;

/// One-content JSON success result.
pub(crate) fn ok_json<T: Serialize>(value: T) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![
        Content::json(value).map_err(to_mcp_error)?,
    ]))
}
