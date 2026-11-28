use rmcp::schemars;
use serde::Deserialize;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct TaskStatusArgs {
    /// The task id (UUID) you're serving.
    pub(crate) task_id: String,
    /// The A2A task state: "working", "input-required" (a question, or
    /// "please review" after an artifact), "completed", "failed", "canceled", "rejected".
    pub(crate) state: String,
    /// The status message: a question, a completion summary, a failure
    /// reason. Required with state "working" (what changed since the last
    /// check-in).
    #[serde(default)]
    pub(crate) note: Option<String>,
    /// Percent done, a whole number from 0 to 100. Required with state
    /// "working", rejected with any other state.
    #[serde(default)]
    #[schemars(range(min = 0, max = 100))]
    pub(crate) progress: Option<u8>,
    /// Set to true when the task waits on your own user. Only valid with state "working";
    /// the next check-in clears it.
    #[serde(default)]
    pub(crate) waiting_on_user: bool,
}
