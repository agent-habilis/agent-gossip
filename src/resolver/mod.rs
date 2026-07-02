use std::fmt;
use std::str::FromStr;

use anyhow::{Result, anyhow};

use crate::protocol::SwarmId;
use crate::protocol::swarm::{Swarm, SwarmIdError};

/// What `join` accepts: a literal `🐝…` swarm id. A shared *string* is not a
/// join target — it derives its own swarm via `ahsw forum`. Classified and
/// validated **once**, at the boundary (clap `FromStr` / MCP entry), so
/// `resolve` matches the variant instead of re-sniffing a `String`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinTarget {
    /// A literal `🐝…` id — resolves with no I/O.
    Swarm(SwarmId),
}

/// A join target that isn't a well-formed swarm id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinTargetError(String);

impl fmt::Display for JoinTargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for JoinTargetError {}

impl FromStr for JoinTarget {
    type Err = JoinTargetError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let trimmed = input.trim();
        match trimmed.parse::<SwarmId>() {
            Ok(id) => Ok(JoinTarget::Swarm(id)),
            // A legacy `ahs…` id is unmistakably a (stale) id — surface the
            // explanatory message rather than the generic forum hint.
            Err(error @ SwarmIdError::LegacyPrefix) => Err(JoinTargetError(error.to_string())),
            // Anything else isn't a `🐝…` id. Point at `forum`, which is what a
            // plain string is for. The hint is meant to be copy-pasted into a
            // shell, so the string is single-quoted (with embedded `'` escaped
            // POSIX-style) — unquoted, whitespace would split into extra args
            // and metacharacters could expand.
            Err(_) => {
                let quoted = format!("'{}'", trimmed.replace('\'', "'\\''"));
                Err(JoinTargetError(format!(
                    "`{trimmed}` is not a swarm id (expected a 🐝… token). To join a \
                     public swarm derived from a shared string, use `ahsw forum {quoted}`."
                )))
            }
        }
    }
}

pub(crate) fn resolve(target: &JoinTarget) -> Result<Swarm> {
    match target {
        JoinTarget::Swarm(id) => id
            .as_str()
            .parse::<Swarm>()
            .map_err(|error| anyhow!("invalid swarm id: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{JoinTarget, resolve};

    #[test]
    fn legacy_ahs_id_reports_stale_prefix() {
        let err = "ahs2fpqZJm7Z7zHDxpNa6jGw7GNqpNpFCSzswQdUsW5B2TnKEz9"
            .parse::<JoinTarget>()
            .unwrap_err();
        assert!(err.to_string().contains("legacy"), "got: {err}");
    }

    #[test]
    fn non_id_string_points_at_forum() {
        let err = "github.com/alice/proj".parse::<JoinTarget>().unwrap_err();
        assert!(
            err.to_string()
                .contains("ahsw forum 'github.com/alice/proj'"),
            "got: {err}"
        );
    }

    #[test]
    fn forum_hint_is_shell_safe() {
        let whitespace_err = "my secret room".parse::<JoinTarget>().unwrap_err();
        assert!(
            whitespace_err
                .to_string()
                .contains("ahsw forum 'my secret room'"),
            "got: {whitespace_err}"
        );
        let quote_err = "it's here".parse::<JoinTarget>().unwrap_err();
        assert!(
            quote_err.to_string().contains(r"ahsw forum 'it'\''s here'"),
            "got: {quote_err}"
        );
    }

    fn known_swarm_id() -> String {
        use crate::protocol::swarm::{Swarm, SwarmConfig, SwarmName};
        Swarm::new(
            [1u8; 32],
            SwarmName::new("test").unwrap(),
            SwarmConfig::loopback(),
        )
        .to_string()
    }

    #[test]
    fn resolve_passthrough_for_valid_swarm_id() {
        let id = known_swarm_id();
        let target: JoinTarget = id.parse().unwrap();
        let swarm = resolve(&target).unwrap();
        assert_eq!(swarm.to_string(), id);
    }

    #[test]
    fn join_target_classifies_valid_id() {
        let id = known_swarm_id();
        assert!(matches!(id.parse::<JoinTarget>(), Ok(JoinTarget::Swarm(_))));
    }
}
