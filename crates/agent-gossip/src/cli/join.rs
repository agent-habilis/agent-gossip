use std::str::FromStr;

use anyhow::Result;
use habilis_network::protocol::{JoinTarget, LockedMesh, Nickname};
use habilis_network::runtime::JoinParams;

use super::args::SharedServerOpts;
use super::run_session::run_session;

/// Join an existing mesh by its identifier, a domain, or a
/// supported git repo URL. The mesh's config (lookups) is decoded from
/// the id — `join` takes no lookup flags.
pub(super) async fn join(
    target: JoinTarget,
    nickname: Option<Nickname>,
    password_flag: Option<super::password_flag::PasswordFlag>,
    shared: SharedServerOpts,
) -> Result<()> {
    let mut password = super::password_flag::resolve_password(password_flag)?;
    // A protected id/invite with the flag absent is a hard error naming the
    // requirement (there is no interactive prompt). Both join-target variants
    // can be password-protected (a mesh id or an invite), so classify
    // either before erroring.
    if password.is_none() {
        let requires: Option<&str> = match &target {
            JoinTarget::Mesh(id) => {
                let mesh = LockedMesh::from_str(id.as_str());
                mesh.is_ok_and(|mesh| mesh.requires_password())
                    .then_some("gossip")
            }
            JoinTarget::Invite(invite) => invite.requires_password().then_some("invite"),
        };
        if let Some(what) = requires {
            password = Some(super::password_flag::require_password(what)?);
        }
    }
    // `join` never advertises — that is a create-time decision.
    let resolved = JoinParams {
        target,
        nickname,
        password,
    }
    .resolve()?;
    Box::pin(run_session(resolved, shared)).await
}
