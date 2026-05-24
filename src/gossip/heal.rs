//! Gossip healer — the sole reconnect primitive for the gossip mesh.

use std::collections::HashSet;
use std::time::Duration;

use iroh::{Endpoint, EndpointId};
use iroh_gossip::api::GossipSender;

use crate::util::tuning::{HEAL_HARD_PROBE_SECS, HEAL_PROBE_SECS};

/// Re-resolve/re-path the seed-derived rendezvous, then re-graft it.
/// The probe is only wanted for that resolution side effect (its
/// connection is discarded) and a cold path can take seconds, so it is
/// detached off the sole event loop; `join_peers` is a cheap enqueue.
async fn heal(
    endpoint: &Endpoint,
    rendezvous_id: EndpointId,
    sender: &GossipSender,
    probe_secs: u64,
) {
    let endpoint = endpoint.clone();
    tokio::spawn(async move {
        let _ =
            crate::lookup::probe_connect(&endpoint, rendezvous_id, Duration::from_secs(probe_secs))
                .await;
    });
    let _ = sender.join_peers(vec![rendezvous_id]).await;
}

/// Gossip healer. iroh-gossip has no built-in reconnect, so this is
/// the sole steady-state recovery primitive — unconditional and
/// cause-agnostic. Every tick it forces iroh to re-resolve/re-path the
/// seed-derived rendezvous, then re-grafts it. Cheap when already
/// meshed (one HyParView control message). A partitioned node is just
/// a cold joiner that kept its subscription and the rendezvous is the
/// creator-independent re-entry point, so this single behavior covers
/// relay churn and ordinary partitions with no gate or blind spot.
///
/// Not sufficient after a process freeze (the timer driving it stalled
/// too); [`tick_heal_hard`] handles that edge.
pub(crate) async fn tick_heal(
    endpoint: &Endpoint,
    rendezvous_id: EndpointId,
    sender: &GossipSender,
) {
    tracing::debug!("heal tick: re-probe + re-graft the rendezvous");
    heal(endpoint, rendezvous_id, sender, HEAL_PROBE_SECS).await;
}

/// Resume-edge re-bootstrap: [`tick_heal`] with a longer probe budget
/// ([`HEAL_HARD_PROBE_SECS`]) so a cold relay re-home after a freeze
/// completes (the steady-state 5s cap routinely aborts it). The caller
/// (`run_heal`) logs the edge and pairs this with clearing
/// `state.meshed` and re-asserting the rendezvous hint.
pub(crate) async fn tick_heal_hard(
    endpoint: &Endpoint,
    rendezvous_id: EndpointId,
    sender: &GossipSender,
) {
    heal(endpoint, rendezvous_id, sender, HEAL_HARD_PROBE_SECS).await;
}

/// Rendezvous-independent re-bridge: re-graft every peer we've ever
/// linked to ([`EventLoopState::known_endpoints`]). [`tick_heal`] only
/// re-grafts the rendezvous, so a node that lost all links because the
/// rendezvous/relay went unreachable stays stranded even when it still
/// holds peers' direct (hole-punched) addresses. The caller fires this
/// only on the isolation signal (zero live links but known peers), so
/// it adds no steady-state churn; `join_peers` is a cheap enqueue and
/// iroh reuses the addresses cached when each peer was first linked.
///
/// [`EventLoopState::known_endpoints`]: crate::daemon::state::EventLoopState::known_endpoints
pub(crate) async fn rebridge_known(sender: &GossipSender, known: &HashSet<EndpointId>) {
    let peers: Vec<EndpointId> = known.iter().copied().collect();
    // `info`, not `debug`: this fires only on the isolation signal (rare,
    // event-driven), and a re-bridge attempt is part of the always-on
    // connectivity story we keep at `info` for post-incident diagnosis —
    // same rationale as the hard-edge `warn` and beacon-migration logs.
    tracing::info!(
        target: "agent_habilis_swarm::gossip",
        count = peers.len(),
        "heal: rendezvous-independent re-bridge (re-dialing known peers)"
    );
    let _ = sender.join_peers(peers).await;
}
