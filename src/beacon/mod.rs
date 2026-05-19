//! The **beacon role**: the runtime that co-hosts the rendezvous
//! endpoint — the creator-independent bootstrap anchor.
//!
//! Concept split (see the Concept Glossary in AGENTS.md): *rendezvous*
//! is the seed-derived **identity** (`protocol::crypto`); *beacon*
//! is the **role** a live member plays by binding and serving that
//! identity. This module owns the role; it never derives the identity.
//!
//! A co-hosting member binds a second iroh endpoint to the shared
//! `rendezvous_secret` and glues it into the local mesh via this
//! process's own participant endpoint, so a cold joiner that dials the
//! seed-derived `rendezvous_id` is shuffled into the full mesh.
//!
//! - **Public:** ephemeral port, discoverable by node id via N0 pkarr.
//!   Every member co-hosts permanently; pkarr is last-writer-wins, so
//!   the record always resolves to a recently-live member.
//! - **Private:** a deterministic loopback port *ladder* (no
//!   pkarr/DNS). Exactly one member per swarm is the beacon: a member
//!   binds the first free rung; on `AddrInUse` it probes the rung's
//!   node id — *ours* ⇒ the beacon already exists, stay a participant;
//!   *foreign* (an unrelated swarm that derived the same port) ⇒ skip
//!   to the next rung. So independent private swarms on one host never
//!   hijack each other, and there is no second same-identity co-host
//!   for a joiner to mis-connect to. On the beacon's death its rung
//!   frees and the next heal/reclaim tick re-elects.
//!
//! The rendezvous endpoint never authors app messages; its node id is
//! filtered out of participant-side neighbor handling.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
use iroh_gossip::proto::TopicId;
use tokio::task::JoinHandle;

use crate::discovery::{add_peer_addr, build_endpoint_for_mode, build_swarm, probe_connect};
use crate::protocol::swarm::{DiscoveryOpts, SwarmMode};
use crate::util::tuning::RENDEZVOUS_PROBE_SECS;

/// Everything [`ensure`] needs to (re)build the rendezvous endpoint.
/// Cheap to clone-hold for the event loop's lifetime.
pub(crate) struct RendezvousParams {
    pub mode: SwarmMode,
    pub topic_id: TopicId,
    /// `rendezvous_secret(seed)` — the shared identity every co-host binds.
    pub secret: SecretKey,
    /// Empty = public (ephemeral, pkarr-discoverable). Non-empty =
    /// private: the deterministic loopback port *ladder* in preference
    /// order. The beacon binds the first free rung; an independent
    /// swarm squatting a rung (seed collision) is skipped instead of
    /// mistaken for our own beacon.
    pub bind_ports: Vec<u16>,
    /// `rendezvous_id`, memoized for neighbor filtering / bootstrap seeding.
    pub id: EndpointId,
    /// The participant's resolved discovery config. The beacon
    /// endpoint must publish `rendezvous_id` to the *same*
    /// address-lookups (or a joiner using only mDNS/DHT could never
    /// resolve it) **and** home on the *same* relay (or the joiner's
    /// relay-pinned rendezvous addr finds nothing) — see
    /// `beacon_discovery`.
    pub discovery: DiscoveryOpts,
}

/// A live co-hosted rendezvous endpoint. Dropping it aborts the task,
/// releasing the endpoint + router (and, private, freeing the
/// deterministic port for the next member to claim).
pub(crate) struct Rendezvous {
    task: JoinHandle<()>,
}

impl Drop for Rendezvous {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Probe a `AddrInUse` private rung: is the listener *our* swarm's
/// rendezvous, or an unrelated swarm that happened to derive the same
/// port? A loopback `connect` to `rendezvous_id` succeeds only if the
/// listener presents that exact node id (iroh validates it during the
/// TLS handshake) — a foreign rendezvous (different key) is rejected,
/// a dead socket times out. Resolves in milliseconds against a live
/// loopback listener; the timeout only guards a pathological socket.
async fn rung_serves_our_swarm(
    participant: &Endpoint,
    rendezvous_id: EndpointId,
    port: u16,
) -> bool {
    let addr = EndpointAddr::new(rendezvous_id)
        .with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
    let ours = probe_connect(
        participant,
        addr,
        Duration::from_secs(RENDEZVOUS_PROBE_SECS),
    )
    .await;
    tracing::trace!(port, ours, "private rung identity-probe");
    ours
}

/// The beacon mirrors the participant's full discovery config —
/// *address-lookups* (so a joiner resolves `rendezvous_id` via
/// whichever it enabled) **and** the relay. The relay must match: the
/// joiner pre-registers `rendezvous_id` at the pinned (or `--relay`)
/// relay (see `daemon::setup::register_rendezvous`), so the beacon has
/// to actually home there or that relay-direct dial finds nothing.
fn beacon_discovery(params: &RendezvousParams) -> DiscoveryOpts {
    DiscoveryOpts {
        mdns: params.discovery.mdns,
        dht: params.discovery.dht,
        relay: params.discovery.relay.clone(),
    }
}

/// Build the rendezvous endpoint (see module docs for the one-beacon /
/// claim-if-free / identity-probe rationale). Public: one
/// ephemeral-port endpoint. Private: bind the first **free** ladder
/// rung; on `AddrInUse`, probe — *ours* ⇒ `None` (stay a participant),
/// *foreign* ⇒ next rung. `None` also covers public build failure /
/// every rung foreign-squatted (≈0); the next tick retries.
async fn build_rendezvous_endpoint(
    params: &RendezvousParams,
    participant: &Endpoint,
) -> Option<Endpoint> {
    let discovery = beacon_discovery(params);
    if params.bind_ports.is_empty() {
        let endpoint =
            build_endpoint_for_mode(params.mode, &discovery, Some(params.secret.clone()), None)
                .await
                .ok();
        if endpoint.is_some() {
            tracing::info!("beacon assumed: bound public rendezvous endpoint (ephemeral port)");
        } else {
            tracing::debug!("public beacon endpoint build failed; next tick retries");
        }
        return endpoint;
    }
    for &port in &params.bind_ports {
        if let Ok(endpoint) = build_endpoint_for_mode(
            params.mode,
            &discovery,
            Some(params.secret.clone()),
            Some(port),
        )
        .await
        {
            tracing::info!(port, "beacon assumed: bound rendezvous ladder rung");
            return Some(endpoint);
        }
        // build failed (AddrInUse): is it our beacon, or a foreign squat?
        if rung_serves_our_swarm(participant, params.id, port).await {
            tracing::debug!(port, "rung already serves our beacon; staying participant");
            return None;
        }
        tracing::debug!(port, "rung squatted by a foreign swarm; trying next rung");
    }
    tracing::debug!("all rendezvous ladder rungs occupied; staying participant");
    None
}

/// Idempotent: a no-op while we co-host and the task is alive;
/// otherwise (never started, or the task died) try to (re)stand-up
/// the rendezvous via [`build_rendezvous_endpoint`]. All outcomes are
/// quiet — the next heal/reclaim tick retries. The bind/probe is
/// synchronous (we must know immediately whether we hold a beacon);
/// the `subscribe_and_join` runs inside the spawned task so the event
/// loop never blocks on it.
pub(crate) async fn ensure(
    params: &RendezvousParams,
    participant: &Endpoint,
    current: &mut Option<Rendezvous>,
) {
    if current
        .as_ref()
        .is_some_and(|rendezvous| !rendezvous.task.is_finished())
    {
        return;
    }
    if current.is_some() {
        tracing::info!("beacon released (co-host task ended); attempting re-stand-up");
    }
    // Finished task = dead beacon; drop it (aborting is a harmless
    // no-op on an already-finished task) before re-arming.
    *current = None;

    let Some(endpoint) = build_rendezvous_endpoint(params, participant).await else {
        // Public: endpoint build failed. Private: every ladder rung is
        // occupied — our swarm's beacon(s) already exist on the ladder
        // (joiners reach them by identity-checked dial). Either way,
        // nothing to do; the next tick retries.
        return;
    };

    let (gossip, router) = build_swarm(endpoint.clone());

    // Register the participant's address so the rendezvous can dial it
    // in private mode (no discovery); a harmless direct hint in public.
    let participant_id = participant.id();
    let _ = add_peer_addr(&endpoint, participant.addr());
    let topic_id = params.topic_id;

    let task = tokio::spawn(async move {
        use std::time::Duration;

        use futures_util::StreamExt as _;

        use crate::util::tuning::{BEACON_MESH_WAIT_SECS, HEAL_INTERVAL_SECS};

        // Keep the gossip frontend + the Router's accept loop alive
        // for the task's lifetime so the rendezvous stays reachable by
        // cold joiners.
        let _endpoint = endpoint;
        let _router = router;

        // Subscribe + bounded-wait to mesh with our own participant so
        // a joiner dialing the rendezvous finds it bridged in, not a
        // bare socket. *Inside the task*, not in `ensure`, so
        // `daemon::run` never blocks here — blocking it stalls
        // in-process two-session setups whose runtime must also drive
        // the peer. Subscribe failure / `joined()` timeout: fall
        // through, the heal loop keeps converging (empty-room safe).
        let Ok(mut topic) = gossip.subscribe(topic_id, vec![participant_id]).await else {
            return;
        };
        // Retain the gossip frontend for the task's lifetime.
        let _gossip = gossip;
        let _ =
            tokio::time::timeout(Duration::from_secs(BEACON_MESH_WAIT_SECS), topic.joined()).await;

        let (sender, mut receiver) = topic.split();
        let mut heal = tokio::time::interval(Duration::from_secs(HEAL_INTERVAL_SECS));
        heal.tick().await; // eat the immediate first tick

        loop {
            tokio::select! {
                event = receiver.next() => {
                    if event.is_none() {
                        break; // topic terminally closed
                    }
                    // App payloads are discarded — this node only
                    // relays the gossip overlay.
                }
                _ = heal.tick() => {
                    // Re-assert the participant link across blips.
                    let _ = sender.join_peers(vec![participant_id]).await;
                }
            }
        }
    });

    tracing::info!("beacon role active: serving the rendezvous");
    *current = Some(Rendezvous { task });
}
