//! Wire types and seed-derived identities.
//!
//! - [`message`]: the `Message` envelope + its value types
//!   (`MessageBody`, `MessageId`) + the size cap.
//! - [`swarm`]: the `💬…` identifier (`SwarmId` shallow string +
//!   `Swarm` decoded form) + `SwarmName` / `SwarmConfig` (lookups) /
//!   relay-ladder parsing.
//! - [`nickname`]: the `Nickname` newtype.
//! - [`crypto`]: seed → rendezvous identity + gossip topic.
//! - [`peer_addr`]: the `PeerInfo` address JSON codec.

pub mod crypto;
mod ident;
pub mod identity;
pub mod message;
pub mod nickname;
pub mod peer_addr;
pub mod seal;
pub mod swarm;
mod wordlist;

pub use message::{
    AppTag, Channel, CorrId, Message, MessageBody, MessageId, MessageKind, PresenceSubtype, Shard,
    ShardGroup,
};
pub use nickname::Nickname;
pub use swarm::SwarmId;
