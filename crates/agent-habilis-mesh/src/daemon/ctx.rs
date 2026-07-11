//! `HandlerCtx`, hoisted here so both `gossip` and `lifecycle` share it.

use iroh::{Endpoint, EndpointId};
use tokio::sync::broadcast;

use crate::gossip::event::NodeSink;
use crate::protocol::identity::Identity;
use crate::protocol::{MeshId, Message, Nickname};
use crate::transport::MeshSender;

/// Immutable loop-level context threaded through every handler.
/// Bundles the refs a handler may need but never mutates itself.
pub struct HandlerCtx<'a> {
    pub sender: &'a MeshSender,
    pub endpoint: &'a Endpoint,
    pub mesh: &'a MeshId,
    pub author: &'a Nickname,
    /// This member's signing identity (Ed25519). Messages we author are
    /// signed with it before broadcast; see [`Identity`].
    pub identity: &'a Identity,
    /// Our own public key as lowercase hex — computed once at loop setup so
    /// the per-message self-echo check is a string compare, not a fresh
    /// key-derivation + allocation on every inbound message.
    pub our_pubkey: &'a str,
    pub max_peers: usize,
    /// Well-known rendezvous endpoint id. Its co-hosted pseudo-node
    /// shows up as a gossip neighbor on participant endpoints; it is
    /// filtered out of peer accounting everywhere it could leak.
    pub rendezvous_id: EndpointId,
    /// Inbound push channel. `Some` only when the in-process consumer wired one
    /// (`DriverMode::InProcess::msg_tx`); every inbound message that survives
    /// the self-author filter is forwarded here before kind routing. `None` for
    /// the CLI, and for an in-process consumer that drains frames some other way.
    pub external_msg_tx: Option<&'a broadcast::Sender<Message>>,
    /// Per-loop generic event sink: the engine emits
    /// [`NodeEvent`](crate::gossip::event::NodeEvent) through it and never names
    /// the app's concrete `Output`. Borrowed for the loop's lifetime; handlers
    /// emit through `ctx.sink`. The app layer reaches its own renderer
    /// separately (it owns the concrete `Output`).
    pub sink: &'a dyn NodeSink,
}

// Manual: `sink` is a `&dyn NodeSink` trait object with no `Debug` bound, so
// the struct can't derive. The identity fields below are what a log reader cares
// about; the borrowed handles are elided.
impl std::fmt::Debug for HandlerCtx<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HandlerCtx")
            .field("mesh", &self.mesh)
            .field("author", &self.author)
            .field("rendezvous_id", &self.rendezvous_id)
            .finish_non_exhaustive()
    }
}
