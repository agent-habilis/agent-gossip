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
//! process's own peer endpoint, so a cold joiner that dials the
//! seed-derived `rendezvous_id` is shuffled into the full mesh.
//!
//! - **Public:** ephemeral port, discoverable by node id via N0 pkarr.
//!   Every member co-hosts permanently; pkarr is last-writer-wins, so
//!   the record always resolves to a recently-live member. Two
//!   `EagerProbed` members can still claim inside each other's probe
//!   window and bind duplicate same-id copies (each capturing its own
//!   bootstrap dial); the event loop's periodic rival re-check shed
//!   (`daemon::event_loop::shed_rival_beacon_if_due`) re-arbitrates, so
//!   the single-co-host invariant holds *eventually*, not at claim time.
//! - **Private:** a deterministic loopback port *ladder* (no
//!   pkarr/DNS). Exactly one member per mesh is the beacon: a member
//!   binds the first free rung; on `AddrInUse` it probes the rung's
//!   node id — *ours* ⇒ the beacon already exists, stay a peer;
//!   *foreign* (an unrelated mesh that derived the same port) ⇒ skip
//!   to the next rung. So independent private meshes on one host never
//!   hijack each other, and there is no second same-identity co-host
//!   for a joiner to mis-connect to. On the beacon's death its rung
//!   frees and the next heal/reclaim tick re-elects.
//!
//! The rendezvous endpoint never authors app messages; its node id is
//! filtered out of peer-side neighbor handling.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use iroh::{Endpoint, EndpointAddr, EndpointId, RelayUrl, SecretKey};
use iroh_gossip::proto::TopicId;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::lookup::{add_peer_addr, build_endpoint, build_mesh, probe_connect};
use crate::protocol::mesh::{LookupOpts, RelayChoice};
use crate::util::tuning::{HEAL_PROBE_SECS, RENDEZVOUS_PROBE_SECS, heal_interval_secs};

/// Everything [`ensure`] needs to (re)build the rendezvous endpoint.
/// Cheap to clone-hold for the event loop's lifetime.
#[derive(Debug)]
pub(crate) struct RendezvousParams {
    pub(crate) topic_id: TopicId,
    /// `rendezvous_secret(seed)` — the shared identity every co-host binds.
    pub(crate) secret: SecretKey,
    /// Empty when the mesh has lookups (ephemeral, address-lookup
    /// discoverable). Non-empty for a loopback-only mesh: the
    /// deterministic loopback port *ladder* in preference order. The
    /// beacon binds the first free rung; an independent mesh squatting a
    /// rung (seed collision) is skipped instead of mistaken for our own
    /// beacon.
    pub(crate) bind_ports: Vec<u16>,
    /// `rendezvous_id`, memoized for neighbor filtering / bootstrap seeding.
    pub(crate) id: EndpointId,
    /// The peer's resolved lookup config. The beacon
    /// endpoint must publish `rendezvous_id` to the *same*
    /// address-lookups (or a joiner using only mDNS/DHT could never
    /// resolve it) — see `beacon_lookups`.
    pub(crate) lookups: LookupOpts,
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
    pub(crate) bootstrap_relay: Option<RelayUrl>,
    /// How the off-loop rung selectors (the backgrounded startup probe
    /// and the beacon co-host's liveness self-monitor) publish a freshly
    /// chosen rung. The event loop holds the matching receiver and, on a
    /// change, updates [`Self::bootstrap_relay`], re-registers the
    /// rendezvous, and re-homes the beacon — so the heavy ladder walk
    /// never runs on the sole event loop.
    pub(crate) rung_tx: watch::Sender<Option<RelayUrl>>,
}

/// A live co-hosted rendezvous endpoint. Dropping it aborts both tasks,
/// releasing the endpoint + router (and, private, freeing the
/// deterministic port for the next member to claim).
pub(crate) struct Rendezvous {
    /// The gossip co-host: subscribes, bridges the rendezvous into the
    /// mesh, and re-asserts the peer link each heal tick.
    task: JoinHandle<()>,
    /// The relay-rung liveness/discovery monitor (`spawn_relay_monitor`).
    /// `None` for private / relay-disabled meshes (nothing to monitor).
    monitor: Option<JoinHandle<()>>,
    /// The co-hosted endpoint itself, retained so [`Self::shed`] can close
    /// it gracefully — a plain drop (task abort) skips the orderly QUIC
    /// close, and the peer's link to its own dead beacon lingers as
    /// a zombie until the idle timeout, stalling the post-shed re-graft.
    endpoint: Endpoint,
}

impl Rendezvous {
    /// Release the beacon *gracefully*: abort the tasks, then close the
    /// endpoint off-loop so every peer holding a link to it (including our
    /// own peer) sees an immediate `NeighborDown` instead of a
    /// zombie link that only dies at the QUIC idle timeout.
    pub(crate) fn shed(self) {
        let endpoint = self.endpoint.clone();
        tokio::spawn(async move {
            endpoint.close().await;
        });
        // `self` drops here, aborting both tasks.
    }
}

impl Drop for Rendezvous {
    fn drop(&mut self) {
        self.task.abort();
        if let Some(monitor) = &self.monitor {
            monitor.abort();
        }
    }
}

/// Probe a `AddrInUse` private rung: is the listener *our* mesh's
/// rendezvous, or an unrelated mesh that happened to derive the same
/// port? A loopback `connect` to `rendezvous_id` succeeds only if the
/// listener presents that exact node id (iroh validates it during the
/// TLS handshake) — a foreign rendezvous (different key) is rejected,
/// a dead socket times out. Resolves in milliseconds against a live
/// loopback listener; the timeout only guards a pathological socket.
async fn rung_serves_our_mesh(peer: &Endpoint, rendezvous_id: EndpointId, port: u16) -> bool {
    let addr = EndpointAddr::new(rendezvous_id)
        .with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
    let ours = probe_connect(peer, addr, Duration::from_secs(RENDEZVOUS_PROBE_SECS)).await;
    tracing::trace!(target: "agent_habilis_mesh::beacon", port, ours, "private rung identity-probe");
    ours
}

/// The beacon mirrors the peer's *address-lookups* (so a joiner
/// resolves `rendezvous_id` via whichever it enabled) but homes on a
/// **single** relay rung — `params.bootstrap_relay`, the first
/// reachable rung of the ladder. Unlike a peer (which spreads
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
            .map_or(RelayChoice::Disabled, |rung| {
                RelayChoice::Custom(vec![rung])
            }),
    }
}

/// Build the rendezvous endpoint (see module docs for the one-beacon /
/// claim-if-free / identity-probe rationale). Public: one
/// ephemeral-port endpoint. Private: bind the first **free** ladder
/// rung; on `AddrInUse`, probe — *ours* ⇒ `None` (stay a peer),
/// *foreign* ⇒ next rung. `None` also covers public build failure /
/// every rung foreign-squatted (≈0); the next tick retries.
async fn build_rendezvous_endpoint(
    params: &RendezvousParams,
    peer: &Endpoint,
    probe_first: bool,
) -> Option<Endpoint> {
    let lookups = beacon_lookups(params);
    if params.bind_ports.is_empty() {
        // Public probe-before-claim — the analog of the private rung
        // identity-probe below. If a beacon already serves the
        // rendezvous, stay a peer rather than binding a second
        // copy of the same `rendezvous_id` on the shared relay, which
        // would collide and capture our own bootstrap dial. Skipped for
        // the eager origin (`probe_first == false`): it has no peers to
        // collide with and must be the beacon from t=0.
        //
        // The probe dials from a **throwaway endpoint**, not `peer`:
        // an ex-holder re-probing after a rival re-check shed carries stale
        // per-id connection state for the shared `rendezvous_id` (the link
        // to its own just-dropped beacon — the reused-endpoint-id pathology
        // of iroh-gossip#10), and a dial from it joins that dead state and
        // times out even while the rival's live addresses sit in the
        // address book. A fresh endpoint has no history to get stuck on.
        if probe_first {
            // Clamped so at most one probe is outstanding per heal tick
            // even when tests shorten the cadence below the probe cap.
            let budget = Duration::from_secs(HEAL_PROBE_SECS.min(heal_interval_secs()));
            let found_rival = match build_endpoint(&lookups, None, None, Vec::new(), None).await {
                Ok(prober) => {
                    let found = probe_connect(&prober, EndpointAddr::new(params.id), budget).await;
                    prober.close().await;
                    found
                }
                // Can't build a prober ⇒ can't tell; claiming blind here
                // risks the duplicate-id collision, so stay a peer
                // and let the next tick retry.
                Err(error) => {
                    tracing::debug!(target: "agent_habilis_mesh::beacon", %error, "rival probe endpoint build failed; next tick retries");
                    return None;
                }
            };
            if found_rival {
                tracing::debug!(target: "agent_habilis_mesh::beacon", "public rendezvous already served by a beacon; staying peer");
                return None;
            }
        }
        let endpoint = build_endpoint(
            &lookups,
            Some(params.secret.clone()),
            None,
            Vec::new(),
            None,
        )
        .await
        .ok();
        if endpoint.is_some() {
            tracing::info!(target: "agent_habilis_mesh::beacon", "beacon assumed: bound public rendezvous endpoint (ephemeral port)");
        } else {
            tracing::debug!(target: "agent_habilis_mesh::beacon", "public beacon endpoint build failed; next tick retries");
        }
        return endpoint;
    }
    for &port in &params.bind_ports {
        if let Ok(endpoint) = build_endpoint(
            &lookups,
            Some(params.secret.clone()),
            Some(port),
            Vec::new(),
            None,
        )
        .await
        {
            tracing::info!(target: "agent_habilis_mesh::beacon", port, "beacon assumed: bound rendezvous ladder rung");
            return Some(endpoint);
        }
        // build failed (AddrInUse): is it our beacon, or a foreign squat?
        if rung_serves_our_mesh(peer, params.id, port).await {
            tracing::debug!(target: "agent_habilis_mesh::beacon", port, "rung already serves our beacon; staying peer");
            return None;
        }
        tracing::debug!(target: "agent_habilis_mesh::beacon", port, "rung squatted by a foreign mesh; trying next rung");
    }
    tracing::debug!(target: "agent_habilis_mesh::beacon", "all rendezvous ladder rungs occupied; staying peer");
    None
}

/// Idempotent: a no-op while we co-host and the task is alive;
/// otherwise (never started, or the task died) try to (re)stand-up
/// the rendezvous via [`build_rendezvous_endpoint`]. All outcomes are
/// quiet — the next heal/reclaim tick retries. The bind/probe is
/// synchronous (we must know immediately whether we hold a beacon);
/// the `subscribe_and_join` runs inside the spawned task so the event
/// loop never blocks on it.
///
/// Returns whether this call stood up a **new** rendezvous (a
/// `None`/dead → live transition) — the edge the event loop's rival
/// re-check scheduling keys on.
pub(crate) async fn ensure(
    params: &RendezvousParams,
    peer: &Endpoint,
    current: &mut Option<Rendezvous>,
    probe_first: bool,
) -> bool {
    if current
        .as_ref()
        .is_some_and(|rendezvous| !rendezvous.task.is_finished())
    {
        return false;
    }
    if current.is_some() {
        tracing::info!(target: "agent_habilis_mesh::beacon", "beacon released (co-host task ended); attempting re-stand-up");
    }
    // Finished task = dead beacon; drop it (aborting is a harmless
    // no-op on an already-finished task) before re-arming.
    *current = None;

    let Some(endpoint) = build_rendezvous_endpoint(params, peer, probe_first).await else {
        // Public: endpoint build failed. Private: every ladder rung is
        // occupied — our mesh's beacon(s) already exist on the ladder
        // (joiners reach them by identity-checked dial). Either way,
        // nothing to do; the next tick retries.
        return false;
    };

    // The rendezvous pseudo-node's active view is overlay plumbing, not the
    // member peer cap, so it stays at the shipped default rather than
    // tracking `--max-peers`.
    // The rendezvous pseudo-node accepts no unicast — it is not a peer.
    let (gossip, router) = build_mesh(
        endpoint.clone(),
        crate::util::consts::GOSSIP_ACTIVE_VIEW_CAPACITY,
        None,
    );

    // Register the peer's address so the rendezvous can dial it
    // in private mode (no lookup); a harmless direct hint in public.
    let peer_id = peer.id();
    let _ = add_peer_addr(&endpoint, peer.addr());
    let topic_id = params.topic_id;

    // Relay-monitor inputs. The monitor runs as its **own** task (below),
    // off both the event loop and this gossip task, so relay probes never
    // stall rendezvous bridging. Spawned whenever relay is enabled
    // (public + a non-empty ladder) — *not* gated on currently holding a
    // rung, so a relay-less beacon keeps probing to rediscover one.
    let ladder = crate::lookup::relay_ladder(&params.lookups.relay);
    let monitors_relay = !params.lookups.is_loopback() && !ladder.is_empty();
    let monitor_endpoint = endpoint.clone();
    let monitor_homed = params.bootstrap_relay.is_some();
    let monitor_rung_tx = params.rung_tx.clone();
    let rendezvous_endpoint = endpoint.clone();

    let task = tokio::spawn(async move {
        use std::time::Duration;

        use futures_util::StreamExt as _;

        use crate::util::tuning::BEACON_MESH_WAIT_SECS;

        // Keep the gossip frontend + the Router's accept loop alive
        // for the task's lifetime so the rendezvous stays reachable by
        // cold joiners.
        let _endpoint = endpoint;
        let _router = router;

        // Subscribe + bounded-wait to mesh with our own peer so
        // a joiner dialing the rendezvous finds it bridged in, not a
        // bare socket. *Inside the task*, not in `ensure`, so
        // `daemon::run` never blocks here — blocking it stalls
        // in-process two-session setups whose runtime must also drive
        // the peer. Subscribe failure / `joined()` timeout: fall
        // through, the heal loop keeps converging (empty-gossip safe).
        let Ok(mut topic) = gossip.subscribe(topic_id, vec![peer_id]).await else {
            return;
        };
        // Retain the gossip frontend for the task's lifetime.
        let _gossip = gossip;
        let _ =
            tokio::time::timeout(Duration::from_secs(BEACON_MESH_WAIT_SECS), topic.joined()).await;

        let (sender, mut receiver) = topic.split();
        let mut heal = tokio::time::interval(Duration::from_secs(heal_interval_secs()));
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
                    // Re-assert the peer link across blips.
                    let _ = sender.join_peers(vec![peer_id]).await;
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

    tracing::info!(target: "agent_habilis_mesh::beacon", "beacon role active: serving the rendezvous");
    *current = Some(Rendezvous {
        task,
        monitor,
        endpoint: rendezvous_endpoint,
    });
    true
}
