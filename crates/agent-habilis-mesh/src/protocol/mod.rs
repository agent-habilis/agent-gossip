//! Wire types and seed-derived identities — the vocabulary every consumer
//! speaks.
//!
//! **Flat by design.** The implementation is split across submodules (message
//! envelope, mesh identifier, nickname, crypto, sealing, address codec), but
//! they are crate-private and everything lands here: a consumer that needed a
//! `Message` and a `MeshName` and a `Password` used to write three import paths
//! into the engine's file layout, which then could not be rearranged without
//! churning every one of them.

pub(crate) mod crypto;
mod ident;
pub(crate) mod identity;
pub(crate) mod mesh;
pub(crate) mod message;
pub(crate) mod nickname;
pub(crate) mod peer_addr;
pub(crate) mod seal;
mod wordlist;

pub use crypto::{Password, TicketAuth, ct_eq};
pub use identity::{Identity, encode_pubkey};
pub use mesh::{
    AdvertiseRequiresReachable, DEFAULT_DIRECTORY, DirectorySelection, LookupOpts, LookupSet, Mesh,
    MeshConfig, MeshId, MeshIdError, MeshName, NameError, RelayChoice, RelayLadder,
    RelayLadderError, RelaySelection, resolve_lookups, validate_advertise,
};
pub use message::{
    AppFrameParams, AppTag, BodyError, Channel, CorrId, IdError, Message, MessageBody, MessageId,
    MessageKind, PresenceSubtype, Shard, ShardGroup, sole_addressee,
};
pub use nickname::{Nickname, NicknameError};
pub use seal::seal_to_body;

/// The invite token itself. Minting lives in
/// [`ops::invite`](crate::ops::invite); decoding is part of the join vocabulary.
pub use crate::invite::InviteTicket;
/// A join token: a literal mesh id, or a creator-minted invite ticket.
pub use crate::resolver::{JoinTarget, JoinTargetError};

// Gate matches the definitions' (`any(test, feature)`): a narrower gate here
// leaves them `pub` but unreachable under a plain `cargo test`.
#[cfg(any(test, feature = "test-fixtures"))]
pub use message::{BuildMsgParams, ChainCtx, build_msg_bytes};
