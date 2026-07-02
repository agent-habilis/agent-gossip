use clap::Parser;

use crate::protocol::{MessageBody, Nickname, SwarmId, TaskId, TaskPhase, TaskPhaseError};

#[derive(Parser, Debug)]
pub(crate) struct TaskOpts {
    /// Swarm identifier (🐝...)
    #[arg(long)]
    pub swarm: SwarmId,

    /// Nickname of the local agent to send as (must have a running join/create session)
    #[arg(long)]
    pub nickname: Nickname,

    /// Addressee: the peer's nickname this task leg is directed at.
    /// For `--phase offer` it must be a current participant, else the
    /// command errors.
    #[arg(long)]
    pub to: Nickname,

    /// Task correlation id (UUID). Mint a fresh one for the opening
    /// `offer`, then pass the same id on every later leg of that task.
    #[arg(long = "task-id")]
    pub task_id: TaskId,

    /// Lifecycle phase: offer (the brief), accept/decline (entry),
    /// context (Q&A), progress (a done/total beat), done (request close +
    /// verification instructions), confirm/change (the verify decision),
    /// cancel.
    #[arg(long, value_parser = parse_phase)]
    pub phase: TaskPhase,

    /// The leg body: the brief for `offer`; a question/answer for
    /// `context`; a `done/total` fraction (e.g. `35/100`) for `progress`;
    /// the summary + verification instructions for `done`; an optional
    /// reason for the rest. An `offer` body must begin with a marker line
    /// on its own — `[[handover]]` (delegate and walk away) or `[[task]]`
    /// (run + report back) — declaring the delegation flow; a missing or
    /// unrecognized marker defaults to task. UTF-8; newlines/tabs allowed,
    /// other control characters rejected.
    #[arg(long)]
    pub text: MessageBody,
}

/// Parse a `--phase` value for clap. Delegates to [`TaskPhase`]'s `FromStr`
/// (the single phase mapping) and stringifies the error for clap's
/// `value_parser`.
fn parse_phase(raw: &str) -> Result<TaskPhase, String> {
    raw.parse()
        .map_err(|error: TaskPhaseError| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{TaskPhase, parse_phase};

    #[test]
    fn parse_phase_accepts_every_phase() {
        assert_eq!(parse_phase("offer").unwrap(), TaskPhase::Offer);
        assert_eq!(parse_phase("accept").unwrap(), TaskPhase::Accept);
        assert_eq!(parse_phase("decline").unwrap(), TaskPhase::Decline);
        assert_eq!(parse_phase("context").unwrap(), TaskPhase::Context);
        assert_eq!(parse_phase("progress").unwrap(), TaskPhase::Progress);
        assert_eq!(parse_phase("done").unwrap(), TaskPhase::Done);
        assert_eq!(parse_phase("confirm").unwrap(), TaskPhase::Confirm);
        assert_eq!(parse_phase("change").unwrap(), TaskPhase::Change);
        assert_eq!(parse_phase("cancel").unwrap(), TaskPhase::Cancel);
    }

    #[test]
    fn parse_phase_rejects_unknown() {
        assert!(parse_phase("bogus").is_err());
    }
}
