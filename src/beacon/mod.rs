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

use iroh::{Endpoint, EndpointAddr, EndpointId, RelayUrl, SecretKey};
use iroh_gossip::proto::TopicId;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::lookup::{add_peer_addr, build_endpoint_for_mode, build_swarm, probe_connect};
use crate::protocol::swarm::{LookupOpts, RelayChoice, SwarmMode};
use crate::util::tuning::{HEAL_PROBE_SECS, RENDEZVOUS_PROBE_SECS};

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
    /// The participant's resolved lookup config. The beacon
    /// endpoint must publish `rendezvous_id` to the *same*
    /// address-lookups (or a joiner using only mDNS/DHT could never
    /// resolve it) — see `beacon_lookups`.
    pub lookups: LookupOpts,
    /// The single relay **rung** the beacon homes on — initialized to
    /// the first ladder rung (optimistic, unprobed) at setup and
    /// corrected off the event loop: a backgrounded startup probe and
    /// the beacon's own liveness self-monitor publish a new rung through
    /// [`Self::rung_tx`], which the event loop applies back here. The
    /// joiner pre-registers `rendezvous_id` at this exact rung
    /// (`daemon::setup::register_rendezvous`), so the beacon must home
    /// here or that relay-direct dial finds nothing. `None` ⇒ no
    /// reachable relay (private mode, relay disabled, or every rung
    /// down) — joiners fall back to mDNS/DHT.
    pub bootstrap_relay: Option<RelayUrl>,
    /// How the off-loop rung selectors (the backgrounded startup probe
    /// and the beacon co-host's liveness self-monitor) publish a freshly
    /// chosen rung. The event loop holds the matching receiver and, on a
    /// change, updates [`Self::bootstrap_relay`], re-registers the
    /// rendezvous, and re-homes the beacon — so the heavy ladder walk
    /// never runs on the sole event loop.
    pub rung_tx: watch::Sender<Option<RelayUrl>>,
}

/// A live co-hosted rendezvous endpoint. Dropping it aborts both tasks,
/// releasing the endpoint + router (and, private, freeing the
/// deterministic port for the next member to claim).
pub(crate) struct Rendezvous {
    /// The gossip co-host: subscribes, bridges the rendezvous into the
    /// mesh, and re-asserts the participant link each heal tick.
    task: JoinHandle<()>,
    /// The relay-rung liveness/discovery monitor ([`spawn_relay_monitor`]).
    /// `None` for private / relay-disabled swarms (nothing to monitor).
    monitor: Option<JoinHandle<()>>,
}

impl Drop for Rendezvous {
    fn drop(&mut self) {
        self.task.abort();
        if let Some(monitor) = &self.monitor {
            monitor.abort();
        }
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

/// The beacon mirrors the participant's *address-lookups* (so a joiner
/// resolves `rendezvous_id` via whichever it enabled) but homes on a
/// **single** relay rung — `params.bootstrap_relay`, the first
/// reachable rung of the ladder. Unlike a participant (which spreads
/// across the whole multi-relay set for resilience), the beacon must be
/// at the one deterministic rung the joiner pre-registers
/// (`daemon::setup::register_rendezvous`), or the relay-direct dial
/// finds nothing. A `None` rung ⇒ relay off for the beacon (joiners use
/// mDNS/DHT).
fn beacon_lookups(params: &RendezvousParams) -> LookupOpts {
    LookupOpts {
        mdns: params.lookups.mdns,
        dht: params.lookups.dht,
        relay: params
            .bootstrap_relay
            .clone()
            .map_or(RelayChoice::Disabled, |rung| RelayChoice::Custom(vec![rung])),
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
    probe_first: bool,
) -> Option<Endpoint> {
    let lookups = beacon_lookups(params);
    if params.bind_ports.is_empty() {
        // Public probe-before-claim — the analog of the private rung
        // identity-probe below. If a beacon already serves the
        // rendezvous, stay a participant rather than binding a second
        // copy of the same `rendezvous_id` on the shared relay, which
        // would collide and capture our own bootstrap dial. Skipped for
        // the eager origin (`probe_first == false`): it has no peers to
        // collide with and must be the beacon from t=0.
        if probe_first
            && probe_connect(
                participant,
                EndpointAddr::new(params.id),
                Duration::from_secs(HEAL_PROBE_SECS),
            )
            .await
        {
            tracing::debug!("public rendezvous already served by a beacon; staying participant");
            return None;
        }
        let endpoint =
            build_endpoint_for_mode(params.mode, &lookups, Some(params.secret.clone()), None)
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
            &lookups,
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
    probe_first: bool,
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

    let Some(endpoint) = build_rendezvous_endpoint(params, participant, probe_first).await else {
        // Public: endpoint build failed. Private: every ladder rung is
        // occupied — our swarm's beacon(s) already exist on the ladder
        // (joiners reach them by identity-checked dial). Either way,
        // nothing to do; the next tick retries.
        return;
    };

    let (gossip, router) = build_swarm(endpoint.clone());

    // Register the participant's address so the rendezvous can dial it
    // in private mode (no lookup); a harmless direct hint in public.
    let participant_id = participant.id();
    let _ = add_peer_addr(&endpoint, participant.addr());
    let topic_id = params.topic_id;

    // Relay-monitor inputs. The monitor runs as its **own** task (below),
    // off both the event loop and this gossip task, so relay probes never
    // stall rendezvous bridging. Spawned whenever relay is enabled
    // (public + a non-empty ladder) — *not* gated on currently holding a
    // rung, so a relay-less beacon keeps probing to rediscover one.
    let ladder = crate::lookup::relay_ladder(&params.lookups.relay);
    let monitors_relay = params.mode == SwarmMode::Public && !ladder.is_empty();
    let monitor_endpoint = endpoint.clone();
    let monitor_homed = params.bootstrap_relay.is_some();
    let monitor_rung_tx = params.rung_tx.clone();

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

    // Relay liveness/discovery, off this gossip task: never stops
    // probing, backs off while relay-less. `None` when relay is disabled
    // (private / empty ladder).
    let monitor = monitors_relay.then(|| {
        crate::lookup::spawn_relay_monitor(monitor_endpoint, ladder, monitor_rung_tx, monitor_homed)
    });

    tracing::info!("beacon role active: serving the rendezvous");
    *current = Some(Rendezvous { task, monitor });
}
