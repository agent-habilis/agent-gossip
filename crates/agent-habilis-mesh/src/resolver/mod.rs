use std::fmt;
use std::str::FromStr;

use anyhow::{Result, anyhow, bail};

use crate::invite::InviteTicket;
use crate::protocol::MeshId;
use crate::protocol::mesh::{Mesh, MeshIdError};

/// What a join accepts: a bare base58 mesh id, or a creator-minted bare
/// base58 invite to an invite-only mesh. A shared *string* is not a join
/// target — it derives its own mesh through the topic path. Classified and
/// validated **once**, at the boundary (clap `FromStr` / MCP entry), so
/// `resolve` matches the variant instead of re-sniffing a `String`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinTarget {
    /// A bare base58 mesh id — resolves with no I/O.
    Mesh(MeshId),
    /// A bare base58 invite to an invite-only mesh — redeemed (signature +
    /// expiry checked, root unwrapped) in `JoinParams`, which holds the
    /// password.
    Invite(InviteTicket),
}

/// Why a string is not a join target. A *classification*, not a rendered
/// remedy: the engine has no commands to point at, so a consumer matches the
/// variant and appends its own "use … instead" hint. [`Unrecognized`] carries
/// the trimmed input for exactly that.
///
/// [`Unrecognized`]: JoinTargetError::Unrecognized
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinTargetError {
    /// Looked like a mesh id (base58) but failed checksum/payload validation.
    MalformedMeshId(MeshIdError),
    /// Decoded as neither an invite nor a mesh id. Carries the trimmed input to
    /// echo back.
    Unrecognized(String),
}

impl fmt::Display for JoinTargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JoinTargetError::MalformedMeshId(error) => error.fmt(formatter),
            JoinTargetError::Unrecognized(input) => write!(
                formatter,
                "`{input}` is not a mesh id or invite (expected a base58 mesh id or invite ticket)"
            ),
        }
    }
}

impl std::error::Error for JoinTargetError {}

impl FromStr for JoinTarget {
    type Err = JoinTargetError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let trimmed = input.trim();
        // Invites and mesh ids share the bare-base58 shape; try invite decode
        // first so a valid invite never falls through into mesh-id validation.
        if let Ok(invite) = InviteTicket::decode(trimmed) {
            return Ok(JoinTarget::Invite(invite));
        }
        match trimmed.parse::<MeshId>() {
            Ok(id) => Ok(JoinTarget::Mesh(id)),
            // Charset/length failures mean "not a mesh id at all" — surface as
            // unrecognized so callers can hint at topic/other paths. Checksum
            // failures are a mistyped id and keep the specific error.
            Err(MeshIdError::InvalidHash) => {
                Err(JoinTargetError::MalformedMeshId(MeshIdError::InvalidHash))
            }
            Err(_) => Err(JoinTargetError::Unrecognized(trimmed.to_owned())),
        }
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
                bail!("this mesh is invite-only — join with an invite ticket, not the bare hash");
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
    use super::{JoinTarget, JoinTargetError, resolve};

    // The consumer builds its own "use the topic command instead" hint off this
    // variant, so the trimmed input has to survive classification verbatim.
    #[test]
    fn a_non_token_string_is_unrecognized_and_echoes_its_input() {
        let error = "  github.com/alice/proj "
            .parse::<JoinTarget>()
            .unwrap_err();
        assert_eq!(
            error,
            JoinTargetError::Unrecognized("github.com/alice/proj".to_owned())
        );
        assert!(
            error.to_string().contains("github.com/alice/proj"),
            "got: {error}"
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
    fn mistyped_mesh_id_reports_an_invalid_gossip_hash() {
        let mut mistyped = known_mesh_id();
        let replacement = if mistyped.ends_with('1') { "2" } else { "1" };
        mistyped.replace_range(mistyped.len() - 1.., replacement);
        let error = mistyped.parse::<JoinTarget>().unwrap_err();
        assert_eq!(error.to_string(), "invalid gossip hash");
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

    /// Ids and tickets used to be told apart by their glyph; now both are bare
    /// Base58Check over the same envelope, so classification rests on framing
    /// alone. A mesh id's second payload byte is `seed[0]` — where an invite
    /// keeps its kind byte — so 1 in 256 mesh ids reaches the invite decoder
    /// looking, for two bytes, exactly like one. It must still come out a
    /// `Mesh`.
    #[test]
    fn a_mesh_id_whose_seed_collides_with_the_invite_kind_byte_still_classifies_as_mesh() {
        use crate::protocol::mesh::{Mesh, MeshConfig, MeshName};
        // `Mesh` frames `version(1) ‖ seed(32) ‖ …`; `InviteTicket` frames
        // `version(1) ‖ kind(1) ‖ …` with `kind == 3`. A seed leading with 3
        // makes the two agree on both leading bytes.
        let id = Mesh::new(
            [3u8; 32],
            MeshName::new("collide").unwrap(),
            MeshConfig::loopback(),
        )
        .to_string();
        assert!(
            matches!(id.parse::<JoinTarget>(), Ok(JoinTarget::Mesh(_))),
            "a mesh id was swallowed by the invite decoder: {id}"
        );
    }
}
