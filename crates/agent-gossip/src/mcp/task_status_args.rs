use rmcp::schemars;
use serde::Deserialize;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct TaskStatusArgs {
    /// The task id (UUID) you're serving.
    pub(crate) task_id: String,
    /// The A2A task state: "working", "input-required" (a question, or
    /// "please review" after an artifact), "completed", "failed", "canceled".
    pub(crate) state: String,
    /// Optional note — the status message (a question, a completion summary,
    /// a failure reason), or a `done/total` progress fraction like "35/100".
    #[serde(default)]
    pub(crate) note: Option<String>,
    /// Set this before you ask your own user something, with the reason. Only
    /// valid with state "working"; the next status clears it.
    #[serde(default)]
    pub(crate) waiting: Option<String>,
}
