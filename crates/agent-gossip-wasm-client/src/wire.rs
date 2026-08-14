//! The chat frame tags, kept identical to the CLI's `src/a2a/wire.rs`.
//!
//! The two chat tags differ only in addressing, and the addressing is what
//! makes them separate tags rather than one tag with an optional `to`: a
//! receiver can then reject a directed BROADCAST (a private line dressed as
//! gossip-wide) and a broadcast MSG (a msg leaked to everyone) on the tag
//! alone.
//!
//! These are duplicated from the app crate rather than shared, because that
//! crate is host-bound and does not build for wasm32. The plan's
//! `agent-gossip-core` extraction is what removes the duplication; until then
//! these strings are the contract.
pub(crate) const BROADCAST: &str = "a2a_broadcast";

/// The extension URI a broadcast payload must carry, from the app crate's
/// `a2a/extensions.rs`. Here rather than beside its one use in `driver.rs`, so
/// the whole duplicated set is in one file and the `agent-gossip-core`
/// extraction cannot miss it.
pub(crate) const EXT_MESH_BROADCAST: &str = "https://agent-habilis.dev/a2a/ext/mesh-broadcast/v1";

// `a2a_msg` is deliberately absent. A directed frame must arrive *sealed* to
// its addressee — that is the whole privacy boundary for a msg, and it needs
// the x25519 machinery the app crate owns. Naming the tag here without being
// able to seal would invite sending a plaintext msg that every forwarding peer
// could read. Broadcast chat is public by definition and needs none of it.
