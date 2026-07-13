use std::fmt;
use std::str::FromStr;

use anyhow::{Result, anyhow, bail};

use crate::invite::InviteTicket;
use crate::protocol::MeshId;
use crate::protocol::mesh::Mesh;
use crate::util::consts::MESH_GLYPH;

/// What `join` accepts: a literal `💬…` mesh id, or a creator-minted `🎟️`
/// invite to an invite-only mesh. A shared *string* is not a join target — it
/// derives its own mesh via `agent-square topic`. Classified and validated
/// **once**, at the boundary (clap `FromStr` / MCP entry), so `resolve` matches
/// the variant instead of re-sniffing a `String`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinTarget {
    /// A literal `💬…` id — resolves with no I/O.
    Mesh(MeshId),
    /// A `🎟️` invite to an invite-only mesh — redeemed (signature + expiry
    /// checked, root unwrapped) in `JoinParams`, which holds the password.
    Invite(InviteTicket),
}

/// A join target that isn't a well-formed mesh id.
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
        if trimmed.starts_with(MESH_GLYPH) {
            return trimmed
                .parse::<MeshId>()
                .map(JoinTarget::Mesh)
                .map_err(|error| JoinTargetError(error.to_string()));
        }
        // The two token brands never collide, so only non-mesh input reaches
        // the invite decoder and topic hint.
        if let Ok(invite) = InviteTicket::decode(trimmed) {
            return Ok(JoinTarget::Invite(invite));
        }
        // Anything else isn't a join token. Point at `topic`, which is what a
        // plain string is for. The hint is meant to be copy-pasted into a shell,
        // so the string is single-quoted (with embedded `'` escaped POSIX-style)
        // — unquoted, whitespace would split into extra args and metacharacters
        // could expand.
        let quoted = format!("'{}'", trimmed.replace('\'', "'\\''"));
        Err(JoinTargetError(format!(
            "`{trimmed}` is not a mesh id or invite (expected a {MESH_GLYPH}… or 🎟️… \
             token). To join a public mesh derived from a shared string, use \
             `agent-square topic {quoted}`."
        )))
    }
}

pub(crate) fn resolve(target: &JoinTarget) -> Result<Mesh> {
    match target {
        JoinTarget::Mesh(id) => {
            let mesh = id
                .as_str()
                .parse::<Mesh>()
                .map_err(|error| anyhow!("invalid mesh id: {error}"))?;
            if mesh.requires_invite() {
                bail!(
                    "this mesh is invite-only — redeem a 🎟️ invite \
                     (`agent-square join <🎟️…>`), not the bare hash"
                );
            }
            Ok(mesh)
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
    fn non_id_string_points_at_topic() {
        let err = "github.com/alice/proj".parse::<JoinTarget>().unwrap_err();
        assert!(
            err.to_string()
                .contains("agent-square topic 'github.com/alice/proj'"),
            "got: {err}"
        );
    }

    #[test]
    fn topic_hint_is_shell_safe() {
        let whitespace_err = "my secret room".parse::<JoinTarget>().unwrap_err();
        assert!(
            whitespace_err
                .to_string()
                .contains("agent-square topic 'my secret room'"),
            "got: {whitespace_err}"
        );
        let quote_err = "it's here".parse::<JoinTarget>().unwrap_err();
        assert!(
            quote_err
                .to_string()
                .contains(r"agent-square topic 'it'\''s here'"),
            "got: {quote_err}"
        );
    }

    fn known_mesh_id() -> String {
        use crate::protocol::mesh::{Mesh, MeshConfig, MeshName};
        Mesh::new(
            [1u8; 32],
            MeshName::new("test").unwrap(),
            MeshConfig::loopback(),
        )
        .to_string()
    }

    #[test]
    fn resolve_passthrough_for_valid_mesh_id() {
        let id = known_mesh_id();
        let target: JoinTarget = id.parse().unwrap();
        let mesh = resolve(&target).unwrap();
        assert_eq!(mesh.to_string(), id);
    }

    #[test]
    fn mistyped_mesh_id_reports_an_invalid_square_hash() {
        let mut mistyped = known_mesh_id();
        let replacement = if mistyped.ends_with('1') { "2" } else { "1" };
        mistyped.replace_range(mistyped.len() - 1.., replacement);
        let error = mistyped.parse::<JoinTarget>().unwrap_err();
        assert_eq!(error.to_string(), "invalid square hash");
    }

    #[test]
    fn join_target_classifies_valid_id() {
        let id = known_mesh_id();
        assert!(matches!(id.parse::<JoinTarget>(), Ok(JoinTarget::Mesh(_))));
    }

    fn invite_only_mesh() -> crate::protocol::mesh::Mesh {
        use crate::protocol::mesh::{Mesh, MeshConfig, MeshName};
        let mut mesh = Mesh::new(
            [5u8; 32],
            MeshName::new("t").unwrap(),
            MeshConfig::loopback(),
        );
        mesh.set_invite();
        mesh
    }

    #[test]
    fn a_bare_invite_only_hash_is_refused_with_a_pointer() {
        // The attack: skip the invite and join with the raw hash. `resolve` must
        // refuse (and never derive the topic, which would panic without a root).
        let id = invite_only_mesh().to_string();
        let target: JoinTarget = id.parse().unwrap();
        let error = resolve(&target).unwrap_err().to_string();
        assert!(error.contains("invite-only"), "got: {error}");
    }

    #[test]
    fn a_minted_invite_classifies_as_invite() {
        let token = crate::invite::mint(&invite_only_mesh(), Some(3600), None).unwrap();
        assert!(matches!(
            token.parse::<JoinTarget>(),
            Ok(JoinTarget::Invite(_))
        ));
    }
}
