//! Wire types and seed-derived identities.
//!
//! - [`message`]: the `Message` envelope + its value types
//!   (`MessageBody`, `MessageId`) + the size cap.
//! - [`swarm`]: the `ahs…` identifier (`SwarmId` shallow string +
//!   `Swarm` decoded form) + `SwarmName` / `SwarmMode` / relay rule.
//! - [`nickname`]: the `Nickname` newtype.
//! - [`crypto`]: seed → rendezvous identity + gossip topic.
//! - [`peer_addr`]: the `PeerInfo` address JSON codec.

pub(crate) mod crypto;
mod ident;
pub(crate) mod message;
pub(crate) mod nickname;
pub(crate) mod peer_addr;
pub(crate) mod swarm;
mod wordlist;

pub(crate) use message::{Message, MessageBody, MessageId, MessageKind, PresenceSubtype};
pub(crate) use nickname::Nickname;
pub(crate) use swarm::SwarmId;
