use std::sync::Arc;

use bytes::Bytes;
use iroh::EndpointId;
use iroh_gossip::api::{ApiError, GossipSender};

use super::spool::SpoolWriter;

/// The swarm's outbound broadcast plane: the gossip sender plus an optional
/// spool mirror. `broadcast` tees every frame to the spool (when `--spool` is
/// active) before handing it to gossip, so one call site feeds both planes and
/// the ~ten existing `.broadcast()` sites need no per-site change. Frames that
/// reach the wire by a path that *bypasses* `broadcast` (a directed
/// unicast/circuit send, or an unmeshed frame buffered for later gossip) mirror
/// explicitly via [`SwarmSender::spool`].
#[derive(Debug)]
pub(crate) struct SwarmSender {
    gossip: GossipSender,
    spool: Option<Arc<SpoolWriter>>,
}

impl SwarmSender {
    pub(crate) fn new(gossip: GossipSender, spool: Option<Arc<SpoolWriter>>) -> Self {
        Self { gossip, spool }
    }

    /// Tee to the spool, then broadcast over gossip. Same signature as
    /// [`GossipSender::broadcast`], so call sites are unchanged.
    pub(crate) async fn broadcast(&self, message: Bytes) -> Result<(), ApiError> {
        if let Some(spool) = &self.spool {
            spool.write(&message);
        }
        self.gossip.broadcast(message).await
    }

    /// Mirror to the spool only — for a frame that reaches the wire without
    /// going through [`broadcast`](Self::broadcast). Teeing a frame that also
    /// gets broadcast is harmless: the content-addressed filename already
    /// exists, so the second write is skipped.
    pub(crate) fn spool(&self, message: &Bytes) {
        if let Some(spool) = &self.spool {
            spool.write(message);
        }
    }

    pub(crate) async fn join_peers(&self, peers: Vec<EndpointId>) -> Result<(), ApiError> {
        self.gossip.join_peers(peers).await
    }

    /// Wait for the spool writer to drain frames queued so far (no-op without a
    /// spool). The shutdown path awaits this, bounded by a timeout, so a
    /// burst-then-quit doesn't lose the tail before `process::exit`.
    pub(crate) async fn flush(&self) {
        if let Some(spool) = &self.spool {
            spool.flush().await;
        }
    }

    /// Swap the inner gossip sender after a topic resubscribe (`gossip::heal`).
    /// The spool writer `Arc` survives the swap, so a healed daemon keeps
    /// mirroring.
    pub(crate) fn replace_gossip(&mut self, gossip: GossipSender) {
        self.gossip = gossip;
    }
}
