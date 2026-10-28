use anyhow::Result;
use rmcp::model::ErrorData as McpError;

/// Parse an A2A task state from its friendly (kebab-case) name — the agent
/// surface, not the A2A wire's `ProtoJSON` `TASK_STATE_*`.
pub(crate) fn parse_task_state(raw: &str) -> Result<crate::a2a::TaskState, McpError> {
    crate::a2a::TaskState::from_friendly(raw).ok_or_else(|| {
        McpError::invalid_params(
            format!(
                "invalid task state '{raw}' (working|input-required|completed|failed|canceled)"
            ),
            None,
        )
    })
}
