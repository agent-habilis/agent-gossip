use anyhow::Result;
use rmcp::model::ErrorData as McpError;

use crate::a2a::TaskId;

/// Parse a task id string into [`TaskId`].
pub(crate) fn parse_task_id(raw: &str) -> Result<TaskId, McpError> {
    raw.parse()
        .map_err(|error: String| McpError::invalid_params(error, None))
}
