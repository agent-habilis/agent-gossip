use bytes::Bytes;
use iroh::EndpointId;
use iroh_gossip::api::{ApiError, GossipSender};

/// The mesh's outbound broadcast plane: a thin wrapper over the gossip sender
/// that survives a topic resubscribe (`gossip::heal`) via [`replace_gossip`],
/// so the ~ten existing `.broadcast()` call sites hold a stable handle across
/// the swap.
#[derive(Debug)]
pub struct MeshSender {
    gossip: GossipSender,
}

impl MeshSender {
    pub(crate) fn new(gossip: GossipSender) -> Self {
        Self { gossip }
    }

    /// Broadcast over gossip. Same signature as [`GossipSender::broadcast`], so
    /// call sites are unchanged.
    /// # Errors
    /// Returns an error if the inputs are invalid or the operation fails.
    pub async fn broadcast(&self, message: Bytes) -> Result<(), ApiError> {
        self.gossip.broadcast(message).await
    }

    pub(crate) async fn join_peers(&self, peers: Vec<EndpointId>) -> Result<(), ApiError> {
        self.gossip.join_peers(peers).await
    }

    /// Swap the inner gossip sender after a topic resubscribe (`gossip::heal`).
    pub(crate) fn replace_gossip(&mut self, gossip: GossipSender) {
        self.gossip = gossip;
    }
}
