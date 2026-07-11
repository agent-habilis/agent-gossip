//! Wire types and seed-derived identities.
//!
//! - [`message`]: the `Message` envelope + its value types
//!   (`MessageBody`, `MessageId`) + the size cap.
//! - [`mesh`]: the `💬…` identifier (`MeshId` shallow string +
//!   `Mesh` decoded form) + `MeshName` / `MeshConfig` (lookups) /
//!   relay-ladder parsing.
//! - [`nickname`]: the `Nickname` newtype.
//! - [`crypto`]: seed → rendezvous identity + gossip topic.
//! - [`peer_addr`]: the `PeerInfo` address JSON codec.

pub mod crypto;
mod ident;
pub mod identity;
pub mod mesh;
pub mod message;
pub mod nickname;
pub mod peer_addr;
pub mod seal;
mod wordlist;

pub use mesh::MeshId;
pub use message::{
    AppFrameParams, AppTag, Channel, CorrId, Message, MessageBody, MessageId, MessageKind,
    PresenceSubtype, Shard, ShardGroup,
};
pub use nickname::Nickname;
