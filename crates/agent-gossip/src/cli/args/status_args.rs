use clap::Args;

use super::session_address::SessionAddress;

#[derive(Args, Debug)]
pub(crate) struct StatusArgs {
    #[command(flatten)]
    pub session: SessionAddress,
    /// The task id (uuid).
    #[arg(long)]
    pub task_id: crate::a2a::TaskId,
    /// The new A2A state.
    #[arg(long, value_parser = parse_state)]
    pub state: crate::a2a::TaskState,
    /// Optional message text (becomes the status message — a question, a
    /// completion summary, a failure reason).
    #[arg(long = "text")]
    pub note: Option<String>,
    /// Mark the task as waiting on your own user, with the reason. Only
    /// valid with `--state working`; the next status clears it.
    #[arg(long)]
    pub waiting: Option<String>,
}

/// Parse the A2A task state a worker can send, from its friendly (kebab-case)
/// name — the agent surface, not the A2A wire's `ProtoJSON` `TASK_STATE_*`.
fn parse_state(raw: &str) -> Result<crate::a2a::TaskState, String> {
    use crate::a2a::TaskState;

    TaskState::parse_friendly(raw)
        .filter(|state| {
            matches!(
                state,
                TaskState::Working
                    | TaskState::InputRequired
                    | TaskState::Completed
                    | TaskState::Failed
                    | TaskState::Canceled
            )
        })
        .ok_or_else(|| {
            format!("invalid task state '{raw}' (working|input-required|completed|failed|canceled)")
        })
}
