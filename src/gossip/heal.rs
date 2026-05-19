//! Gossip healer — the sole reconnect primitive for the gossip mesh.

use std::time::Duration;

use iroh::{Endpoint, EndpointId};
use iroh_gossip::api::GossipSender;

use crate::util::tuning::HEAL_PROBE_SECS;

/// Gossip healer. iroh-gossip has no built-in reconnect, so this is
/// the sole recovery primitive — unconditional and cause-agnostic.
/// Every tick it forces iroh to re-resolve/re-path the seed-derived
/// rendezvous, then re-grafts it. Cheap when already meshed (one
/// HyParView control message). A partitioned node is just a cold
/// joiner that kept its subscription and the rendezvous is the
/// creator-independent re-entry point, so this single behavior covers
/// sleep/wake, relay churn, and any partition with no gate or blind
/// spot.
pub(crate) async fn tick_heal(
    endpoint: &Endpoint,
    rendezvous_id: EndpointId,
    sender: &GossipSender,
) {
    // Detached: the connect-probe is a pure resolution side effect
    // (its connection is discarded) and a cold path can take up to
    // `HEAL_PROBE_SECS`, which must not stall the sole event loop.
    // `join_peers` is a cheap actor enqueue, so it stays inline.
    tracing::debug!("heal tick: re-probe + re-graft the rendezvous");
    let endpoint = endpoint.clone();
    tokio::spawn(async move {
        let _ = crate::discovery::probe_connect(
            &endpoint,
            rendezvous_id,
            Duration::from_secs(HEAL_PROBE_SECS),
        )
        .await;
    });
    let _ = sender.join_peers(vec![rendezvous_id]).await;
}
