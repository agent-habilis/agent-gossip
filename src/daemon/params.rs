//! The shared create/join intent, resolved once for every frontend.
//!
//! The CLI (`clap`), the embed facade (public API), and the MCP server
//! (`serde`) each have their own native input struct, but they all want the
//! same thing: turn a name + config + nickname (create) or a target +
//! nickname (join) into a [`SetupKind`] plus the resolved author and the
//! directory to advertise into. [`CreateParams`]/[`JoinParams`] are that
//! common shape; `resolve` is the single place the nickname default, the
//! advertise validation, and the target resolution live — instead of three
//! near-identical copies in `cli`, `embed`, and `mcp`.

use anyhow::Result;

use crate::protocol::Nickname;
use crate::protocol::swarm::{
    AdvertiseRequiresReachable, DirectorySelection, SwarmConfig, SwarmName, validate_advertise,
};
use crate::resolver::{self, JoinTarget};

use super::setup::SetupKind;

/// The create intent, before resolution. Each frontend builds this from its
/// own input struct (the lookups are already resolved into `config`).
pub(crate) struct CreateParams {
    pub name: SwarmName,
    /// `None` ⇒ a random `word-word` nickname is minted in `resolve`.
    pub nickname: Option<Nickname>,
    pub config: SwarmConfig,
    pub advertise: DirectorySelection,
}

/// The join intent, before resolution.
pub(crate) struct JoinParams {
    pub target: JoinTarget,
    pub nickname: Option<Nickname>,
}

/// A resolved create/join, ready to hand to
/// [`setup_swarm`](super::setup::setup_swarm). `advertise_directory` is the
/// directory to re-broadcast into (`create --advertise`), or `None`.
pub(crate) struct Resolved {
    pub kind: SetupKind,
    pub author: Nickname,
    pub advertise_directory: Option<SwarmName>,
}

impl CreateParams {
    /// Validate the advertise request against the config, default the
    /// nickname, and build the create [`SetupKind`].
    ///
    /// # Errors
    /// Returns [`AdvertiseRequiresReachable`] if `advertise` is set on a
    /// loopback-only swarm.
    pub(crate) fn resolve(self) -> Result<Resolved, AdvertiseRequiresReachable> {
        validate_advertise(&self.advertise, &self.config.lookups)?;
        let advertise_directory = self.advertise.directory();
        Ok(Resolved {
            kind: SetupKind::Create {
                name: self.name,
                config: self.config,
                advertise: advertise_directory.clone(),
            },
            author: self.nickname.unwrap_or_else(Nickname::random),
            advertise_directory,
        })
    }
}

impl JoinParams {
    /// Resolve the target (`🐝…` id / domain / git URL) into a [`Swarm`]
    /// and default the nickname. `join` never advertises.
    ///
    /// # Errors
    /// Fails if the target cannot be resolved (bad id, unreachable
    /// well-known, malformed JSON).
    ///
    /// [`Swarm`]: crate::protocol::swarm::Swarm
    pub(crate) async fn resolve(self) -> Result<Resolved> {
        let swarm = resolver::resolve(&self.target).await?;
        Ok(Resolved {
            kind: SetupKind::Join { swarm },
            author: self.nickname.unwrap_or_else(Nickname::random),
            advertise_directory: None,
        })
    }
}
