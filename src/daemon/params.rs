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

use std::fmt;

use anyhow::Result;

use crate::protocol::Nickname;
use crate::protocol::crypto::Password;
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
    /// Password to protect the swarm with; the verifier is baked into the
    /// id at setup (the salt is the seed, minted there).
    pub password: Option<Password>,
}

/// The join intent, before resolution.
pub(crate) struct JoinParams {
    pub target: JoinTarget,
    pub nickname: Option<Nickname>,
    /// Password for a passworded id; verified against the id's verifier in
    /// `resolve` (before any network).
    pub password: Option<Password>,
}

/// A passworded id was joined without a password. Typed so callers can
/// classify it — the MCP server maps it to `invalid_params` (it never
/// prompts), the CLI prompts on a TTY before resolving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PasswordRequired;

impl fmt::Display for PasswordRequired {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("this swarm is password-protected — pass --password")
    }
}

impl std::error::Error for PasswordRequired {}

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
                password: self.password,
            },
            author: self.nickname.unwrap_or_else(Nickname::random),
            advertise_directory,
        })
    }
}

impl JoinParams {
    /// Resolve the `🐝…` id target into a [`Swarm`], verify the password
    /// against the id's verifier (locally — a wrong password fails here,
    /// before any network), and default the nickname. `join` never
    /// advertises.
    ///
    /// # Errors
    /// The id cannot be decoded into a [`Swarm`]; the id requires a password
    /// and none was given ([`PasswordRequired`], typed for the frontends);
    /// the password is wrong; or a password was given for a passwordless id.
    ///
    /// [`Swarm`]: crate::protocol::swarm::Swarm
    pub(crate) fn resolve(self) -> Result<Resolved> {
        let mut swarm = resolver::resolve(&self.target)?;
        match (&self.password, swarm.requires_password()) {
            (None, true) => return Err(PasswordRequired.into()),
            (None, false) => {}
            // ~100ms of Argon2id, accepted synchronously: this runs once
            // per join, before the daemon exists.
            (Some(password), _) => swarm.apply_password(password)?,
        }
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
            password: None,
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
