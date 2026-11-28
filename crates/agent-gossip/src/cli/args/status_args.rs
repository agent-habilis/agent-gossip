use clap::Args;

use super::session_address::SessionAddress;
use crate::a2a::status_update_params::Progress;

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
    /// Message text (becomes the status message — a question, a completion
    /// summary, a failure reason). Required with `--state working`: what
    /// changed since the last check-in.
    #[arg(long, alias = "note")]
    pub text: Option<String>,
    /// Percent done, a whole number from 0 to 100. Required with
    /// `--state working`, rejected with any other state.
    #[arg(long)]
    pub progress: Option<Progress>,
    /// Say that the task waits on your own user. Only valid with
    /// `--state working`; the next check-in clears it.
    #[arg(long)]
    pub waiting_on_user: bool,
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
