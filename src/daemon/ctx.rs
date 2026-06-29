//! `HandlerCtx`, hoisted here so both `gossip` and `lifecycle` share it.

use iroh::{Endpoint, EndpointId};
use iroh_gossip::api::GossipSender;
use tokio::sync::broadcast;

use crate::output;
use crate::protocol::identity::Identity;
use crate::protocol::{Message, Nickname, SwarmId};

/// Immutable loop-level context threaded through every handler.
/// Bundles the refs a handler may need but never mutates itself.
pub(crate) struct HandlerCtx<'a> {
    pub sender: &'a GossipSender,
    pub endpoint: &'a Endpoint,
    pub swarm: &'a SwarmId,
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
    /// Embed facade push channel. `Some` only when a `SwarmSession`
    /// drives the loop; every inbound message that survives the
    /// self-author filter is forwarded here before kind routing.
    /// `None` for CLI/MCP.
    pub external_msg_tx: Option<&'a broadcast::Sender<Message>>,
    /// Per-loop output sink, so multiple in-process sessions don't share
    /// one global. Borrowed for the loop's lifetime; handlers read it
    /// through `ctx.output`.
    pub output: &'a output::Output,
}
