use std::fmt;
use std::str::FromStr;

use anyhow::{Result, anyhow, bail};

use crate::invite::InviteTicket;
use crate::protocol::SwarmId;
use crate::protocol::swarm::{Swarm, SwarmIdError};
use crate::util::consts::SWARM_GLYPH;

/// What `join` accepts: a literal `💬…` swarm id, or a creator-minted `🎟️`
/// invite to an invite-only swarm. A shared *string* is not a join target — it
/// derives its own swarm via `agent-gossip topic`. Classified and validated
/// **once**, at the boundary (clap `FromStr` / MCP entry), so `resolve` matches
/// the variant instead of re-sniffing a `String`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinTarget {
    /// A literal `💬…` id — resolves with no I/O.
    Swarm(SwarmId),
    /// A `🎟️` invite to an invite-only swarm — redeemed (signature + expiry
    /// checked, root unwrapped) in `JoinParams`, which holds the password.
    Invite(InviteTicket),
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
            // explanatory message rather than the generic topic hint.
            Err(error @ SwarmIdError::LegacyPrefix) => Err(JoinTargetError(error.to_string())),
            // Not a `💬…` id — try a `🎟️` invite before falling back to the
            // topic hint (the two brands never collide, so a clean classify).
            Err(_) => {
                if let Ok(invite) = InviteTicket::decode(trimmed) {
                    return Ok(JoinTarget::Invite(invite));
                }
                // Anything else isn't a join token. Point at `topic`, which is
                // what a plain string is for. The hint is meant to be
                // copy-pasted into a shell, so the string is single-quoted (with
                // embedded `'` escaped POSIX-style) — unquoted, whitespace would
                // split into extra args and metacharacters could expand.
                let quoted = format!("'{}'", trimmed.replace('\'', "'\\''"));
                Err(JoinTargetError(format!(
                    "`{trimmed}` is not a swarm id or invite (expected a {SWARM_GLYPH}… or 🎟️… \
                     token). To join a public swarm derived from a shared string, use \
                     `agent-gossip topic {quoted}`."
                )))
            }
        }
    }
}

pub(crate) fn resolve(target: &JoinTarget) -> Result<Swarm> {
    match target {
        JoinTarget::Swarm(id) => {
            let swarm = id
                .as_str()
                .parse::<Swarm>()
                .map_err(|error| anyhow!("invalid swarm id: {error}"))?;
            if swarm.requires_invite() {
                bail!(
                    "this swarm is invite-only — redeem a 🎟️ invite \
                     (`agent-gossip join <🎟️…>`), not the bare hash"
                );
            }
            Ok(swarm)
        }
        // An invite carries the join key and needs the password to unwrap it, so
        // it is redeemed in `JoinParams::resolve`; `resolve` never sees it alone.
        JoinTarget::Invite(_) => {
            bail!("internal: an invite target must be redeemed via JoinParams")
        }
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
    fn non_id_string_points_at_topic() {
        let err = "github.com/alice/proj".parse::<JoinTarget>().unwrap_err();
        assert!(
            err.to_string()
                .contains("agent-gossip topic 'github.com/alice/proj'"),
            "got: {err}"
        );
    }

    #[test]
    fn topic_hint_is_shell_safe() {
        let whitespace_err = "my secret room".parse::<JoinTarget>().unwrap_err();
        assert!(
            whitespace_err
                .to_string()
                .contains("agent-gossip topic 'my secret room'"),
            "got: {whitespace_err}"
        );
        let quote_err = "it's here".parse::<JoinTarget>().unwrap_err();
        assert!(
            quote_err
                .to_string()
                .contains(r"agent-gossip topic 'it'\''s here'"),
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

    fn invite_only_swarm() -> crate::protocol::swarm::Swarm {
        use crate::protocol::swarm::{Swarm, SwarmConfig, SwarmName};
        let mut swarm = Swarm::new(
            [5u8; 32],
            SwarmName::new("t").unwrap(),
            SwarmConfig::loopback(),
        );
        swarm.set_invite();
        swarm
    }

    #[test]
    fn a_bare_invite_only_hash_is_refused_with_a_pointer() {
        // The attack: skip the invite and join with the raw hash. `resolve` must
        // refuse (and never derive the topic, which would panic without a root).
        let id = invite_only_swarm().to_string();
        let target: JoinTarget = id.parse().unwrap();
        let error = resolve(&target).unwrap_err().to_string();
        assert!(error.contains("invite-only"), "got: {error}");
    }

    #[test]
    fn a_minted_invite_classifies_as_invite() {
        let token = crate::invite::mint(&invite_only_swarm(), Some(3600), None).unwrap();
        assert!(matches!(
            token.parse::<JoinTarget>(),
            Ok(JoinTarget::Invite(_))
        ));
    }
}
