//! Gossip healer — the sole reconnect primitive for the gossip mesh.

use std::time::{Duration, Instant};

use iroh::{Endpoint, EndpointId};
use iroh_gossip::api::GossipSender;

use crate::daemon::ctx::HandlerCtx;
use crate::daemon::state::EventLoopState;
use crate::util::bounded_fifo_set::BoundedFifoSet;
use crate::util::tuning::HEAL_HARD_PROBE_SECS;

/// Hard-heal body: re-resolve/re-path the seed-derived rendezvous via a
/// detached connect-probe (only wanted for that resolution side effect;
/// a cold path can take seconds, so it never runs on the sole event
/// loop), then re-graft it. `join_peers` is a cheap enqueue.
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
    // A failing re-graft is the recovery path failing — it must be loud.
    // The 11h roster-collapse soak ran 2,596 of these with zero log signal.
    if let Err(error) = sender.join_peers(vec![rendezvous_id]).await {
        tracing::warn!(
            target: "agent_gossip::gossip",
            %error,
            "heal: rendezvous re-graft request failed"
        );
    }
}

/// Gossip healer. iroh-gossip has no built-in reconnect, so this is
/// the sole steady-state recovery primitive: re-graft the seed-derived
/// rendezvous via `join_peers` (the gossip actor dials it with the
/// endpoint's address lookups). A partitioned node is just a cold
/// joiner that kept its subscription and the rendezvous is the
/// creator-independent re-entry point.
///
/// The caller runs this **only while the rendezvous link is down**, and
/// it deliberately does NOT connect-probe: every `GOSSIP_ALPN`
/// connection gets *adopted* by the beacon's gossip as the member's
/// peer connection, so a probe alongside the graft supersedes — and on
/// close, kills — the very link `join_peers` is forming. That race was
/// the 2026-05-30 soak's once-per-tick rendezvous flap (reproduced in
/// `code-review/2026-06-12 … repro-m2-probe-churn`). The long probe
/// survives where cold re-resolution is genuinely needed:
/// [`tick_heal_hard`] (resume edge) and the beacon's own probes.
pub(crate) async fn tick_heal(rendezvous_id: EndpointId, sender: &GossipSender) {
    tracing::info!(
        target: "agent_gossip::gossip",
        "heal tick: re-graft the rendezvous"
    );
    if let Err(error) = sender.join_peers(vec![rendezvous_id]).await {
        tracing::warn!(
            target: "agent_gossip::gossip",
            %error,
            "heal: rendezvous re-graft request failed"
        );
    }
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
pub(crate) async fn rebridge_known(sender: &GossipSender, known: &BoundedFifoSet<EndpointId>) {
    let peers: Vec<EndpointId> = known.iter().copied().collect();
    // `info`, not `debug`: this fires only on the isolation signal (rare,
    // event-driven), and a re-bridge attempt is part of the always-on
    // connectivity story we keep at `info` for post-incident diagnosis —
    // same rationale as the hard-edge `warn` and beacon-migration logs.
    tracing::info!(
        target: "agent_gossip::gossip",
        count = peers.len(),
        "heal: rendezvous-independent re-bridge (re-dialing known peers)"
    );
    if let Err(error) = sender.join_peers(peers).await {
        tracing::warn!(
            target: "agent_gossip::gossip",
            %error,
            "heal: re-bridge graft request failed"
        );
    }
}

/// Starvation recovery: the heal arm detected verified inbound silence
/// past the threshold while real peers are known
/// ([`EventLoopState::starvation_due`]) — the roster-collapse signature,
/// where the overlay is wedged but heal's rendezvous re-graft alone
/// never re-admits us. Degrade `meshed` (outbound buffers until traffic
/// proves the mesh again), reset the per-peer throttles, re-dial every
/// remembered peer directly, and re-announce (`joined` + `PeerInfo`) so
/// peers that evicted us re-seed their rosters the moment a link
/// re-forms. Loud by design: this is the watchdog the 11h silent
/// partition lacked.
pub(crate) async fn recover_from_starvation(state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
    let starved_secs = state.last_inbound_at.elapsed().as_secs();
    tracing::warn!(
        target: "agent_gossip::gossip",
        starved_secs,
        trips = state.recovery_trips,
        "mesh starvation: no inbound traffic; re-bridging known peers and re-announcing"
    );
    ctx.output.info(&format!(
        "mesh starvation: no traffic for {starved_secs}s; attempting recovery"
    ));
    state.note_degraded();
    state.relink.clear();
    state.peerinfo.clear();
    rebridge_known(ctx.sender, &state.known_endpoints).await;
    super::broadcast::announce_arrival(
        ctx.sender,
        ctx.swarm,
        ctx.author,
        ctx.identity,
        ctx.endpoint,
    )
    .await;
    let now = Instant::now();
    state.last_sent_at = now;
    state.note_recovery(now);
}
