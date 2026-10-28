use std::fmt;

use serde::{Deserialize, Serialize};

/// A2A v1.0 `TaskState`, `ProtoJSON` `SCREAMING_SNAKE` with the `TASK_STATE_`
/// prefix on the wire; [`TaskState::as_str`] keeps the friendly kebab name for
/// our own `--output json` stream and CLI, which are the agent API, not A2A.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    #[serde(rename = "TASK_STATE_UNSPECIFIED")]
    Unspecified,
    #[serde(rename = "TASK_STATE_SUBMITTED")]
    Submitted,
    #[serde(rename = "TASK_STATE_WORKING")]
    Working,
    #[serde(rename = "TASK_STATE_INPUT_REQUIRED")]
    InputRequired,
    #[serde(rename = "TASK_STATE_COMPLETED")]
    Completed,
    #[serde(rename = "TASK_STATE_CANCELED")]
    Canceled,
    #[serde(rename = "TASK_STATE_FAILED")]
    Failed,
    #[serde(rename = "TASK_STATE_REJECTED")]
    Rejected,
    #[serde(rename = "TASK_STATE_AUTH_REQUIRED")]
    AuthRequired,
}

impl TaskState {
    /// Terminal states end a task for good: no leg may follow, and a frame
    /// that tries is dropped (resurrect-after-terminal is an attack shape).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskState::Completed | TaskState::Canceled | TaskState::Failed | TaskState::Rejected
        )
    }

    /// The friendly kebab name for display lines, our `--output json` stream,
    /// and the `agent-gossip a2a status --state` flag — the agent-facing surface, kept
    /// readable while the A2A wire carries the `ProtoJSON` `TASK_STATE_*` form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Unspecified => "unspecified",
            TaskState::Submitted => "submitted",
            TaskState::Working => "working",
            TaskState::InputRequired => "input-required",
            TaskState::Completed => "completed",
            TaskState::Canceled => "canceled",
            TaskState::Failed => "failed",
            TaskState::Rejected => "rejected",
            TaskState::AuthRequired => "auth-required",
        }
    }

    /// Parse the friendly kebab name (the CLI `--state` surface).
    #[must_use]
    pub fn from_friendly(raw: &str) -> Option<Self> {
        Some(match raw {
            "submitted" => TaskState::Submitted,
            "working" => TaskState::Working,
            "input-required" => TaskState::InputRequired,
            "completed" => TaskState::Completed,
            "canceled" => TaskState::Canceled,
            "failed" => TaskState::Failed,
            "rejected" => TaskState::Rejected,
            "auth-required" => TaskState::AuthRequired,
            _ => return None,
        })
    }
}

impl fmt::Display for TaskState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}
