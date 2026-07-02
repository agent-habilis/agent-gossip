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
    AdvertiseRequiresReachable, DirectorySelection, LookupOpts, Swarm, SwarmConfig, SwarmName,
    validate_advertise,
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

/// The forum intent: a swarm derived deterministically from a shared string,
/// always public. Name + config are derived, not supplied — the string alone
/// determines the swarm.
pub(crate) struct ForumParams {
    pub string: String,
    /// `None` ⇒ a random `word-word` nickname is minted in `resolve`.
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
    /// Resolve the `🐝…` id target into a [`Swarm`] and default the nickname.
    /// `join` never advertises.
    ///
    /// # Errors
    /// Fails if the id cannot be decoded into a [`Swarm`].
    ///
    /// [`Swarm`]: crate::protocol::swarm::Swarm
    pub(crate) fn resolve(self) -> Result<Resolved> {
        let swarm = resolver::resolve(&self.target)?;
        Ok(Resolved {
            kind: SetupKind::Join { swarm },
            author: self.nickname.unwrap_or_else(Nickname::random),
            advertise_directory: None,
        })
    }
}

/// The canonical [`Swarm`] a forum string derives — always the public preset.
/// The single source of that derivation *and* of the empty/whitespace-string
/// guard, shared by [`ForumParams::resolve`] and the MCP idempotency check so
/// neither can drift — an empty string would otherwise silently derive one
/// globally-fixed swarm that every empty-string caller lands in. (The clap
/// `value_parser` on `ahsw forum` re-checks emptiness only to surface it as a
/// parse-time usage error.)
pub(crate) fn derive_forum_swarm(string: &str) -> Result<Swarm> {
    if string.trim().is_empty() {
        anyhow::bail!("forum string must not be empty");
    }
    Ok(Swarm::from_topic(
        string,
        SwarmConfig {
            lookups: LookupOpts::public_preset(),
        },
    ))
}

impl ForumParams {
    /// Derive the swarm from the string (always the public preset) and default
    /// the nickname. A forum never advertises. The empty/whitespace-string
    /// guard lives in [`derive_forum_swarm`], which every frontend (CLI,
    /// embed, MCP) funnels through.
    ///
    /// # Errors
    /// Fails if the string is empty or whitespace-only.
    pub(crate) fn resolve(self) -> Result<Resolved> {
        let swarm = derive_forum_swarm(&self.string)?;
        Ok(Resolved {
            kind: SetupKind::Forum { swarm },
            author: self.nickname.unwrap_or_else(Nickname::random),
            advertise_directory: None,
        })
    }
}
