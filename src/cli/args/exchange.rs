//! `exchange` command args: send one leg of an exchange to a specific
//! peer via the running daemon's IPC socket. An exchange is a directed, phased
//! exchange correlated by `exchange_id`; `handover` is one behavior built on it
//! (see the manual's exchange workflow).

use clap::Parser;

use crate::protocol::{
    ExchangeId, ExchangeKind, ExchangeKindError, ExchangePhase, ExchangePhaseError, MessageBody,
    Nickname, SwarmId,
};

#[derive(Parser, Debug)]
pub(crate) struct ExchangeOpts {
    /// Swarm identifier (🐝...)
    #[arg(long)]
    pub swarm: SwarmId,

    /// Nickname of the local agent to send as (must have a running join/create session)
    #[arg(long)]
    pub nickname: Nickname,

    /// Addressee: the peer's nickname this exchange leg is directed at.
    /// For `--phase offer` it must be a current participant, else the
    /// command errors.
    #[arg(long)]
    pub to: Nickname,

    /// Exchange correlation id (UUID). Mint a fresh one for the opening
    /// `offer`, then pass the same id on every later leg of that exchange.
    #[arg(long = "exchange-id")]
    pub exchange_id: ExchangeId,

    /// Behavior: `handover` (delegate a task/plan, no result) or `task` (run + report + verify).
    #[arg(long, value_parser = parse_kind)]
    pub kind: ExchangeKind,

    /// Lifecycle phase: offer (the brief), accept/decline (entry),
    /// context (Q&A), progress (a done/total beat), done (request close +
    /// verification instructions), confirm/change (the verify decision),
    /// cancel.
    #[arg(long, value_parser = parse_phase)]
    pub phase: ExchangePhase,

    /// The leg body: the brief for `offer`; a question/answer for
    /// `context`; a `done/total` fraction (e.g. `35/100`) for `progress`;
    /// the summary + verification instructions for `done`; an optional
    /// reason for the rest. UTF-8; newlines/tabs allowed, other control
    /// characters rejected.
    #[arg(long)]
    pub text: MessageBody,
}

/// Parse a `--phase` value for clap. Delegates to [`ExchangePhase`]'s `FromStr`
/// (the single phase mapping) and stringifies the error for clap's
/// `value_parser`.
fn parse_phase(raw: &str) -> Result<ExchangePhase, String> {
    raw.parse()
        .map_err(|error: ExchangePhaseError| error.to_string())
}

/// Parse a `--kind` value for clap (delegates to [`ExchangeKind`]'s `FromStr`).
fn parse_kind(raw: &str) -> Result<ExchangeKind, String> {
    raw.parse()
        .map_err(|error: ExchangeKindError| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{ExchangeKind, ExchangePhase, parse_kind, parse_phase};

    #[test]
    fn parse_phase_accepts_every_phase() {
        assert_eq!(parse_phase("offer").unwrap(), ExchangePhase::Offer);
        assert_eq!(parse_phase("accept").unwrap(), ExchangePhase::Accept);
        assert_eq!(parse_phase("decline").unwrap(), ExchangePhase::Decline);
        assert_eq!(parse_phase("context").unwrap(), ExchangePhase::Context);
        assert_eq!(parse_phase("progress").unwrap(), ExchangePhase::Progress);
        assert_eq!(parse_phase("done").unwrap(), ExchangePhase::Done);
        assert_eq!(parse_phase("confirm").unwrap(), ExchangePhase::Confirm);
        assert_eq!(parse_phase("change").unwrap(), ExchangePhase::Change);
        assert_eq!(parse_phase("cancel").unwrap(), ExchangePhase::Cancel);
    }

    #[test]
    fn parse_phase_rejects_unknown() {
        assert!(parse_phase("bogus").is_err());
    }

    #[test]
    fn parse_kind_accepts_both() {
        assert_eq!(parse_kind("handover").unwrap(), ExchangeKind::Handover);
        assert_eq!(parse_kind("task").unwrap(), ExchangeKind::Task);
        assert!(parse_kind("bogus").is_err());
    }
}
