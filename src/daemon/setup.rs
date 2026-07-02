use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use iroh::{Endpoint, EndpointAddr, RelayUrl};
use rand::RngCore;
use tokio::sync::watch;

use crate::lookup::{
    add_peer_addr, build_participant_endpoint, build_swarm, relay_ladder, select_bootstrap_rung,
};
use crate::output;
use crate::protocol::crypto::{derive_topic_id, rendezvous_secret};
use crate::protocol::swarm::{LookupOpts, Swarm, SwarmConfig, SwarmName};
use crate::protocol::{Nickname, SwarmId};
use crate::util::tuning::RELAY_RUNG_PROBE_SECS;

use crate::beacon::RendezvousParams;
use crate::lifecycle;

use super::{CoHostPolicy, DriverMode, EventLoopConfig};

/// What kind of swarm we're setting up — either minting a new one
/// (create) or attaching to an existing one (join).
pub(crate) enum SetupKind {
    Create {
        name: SwarmName,
        /// The swarm-wide config (lookups) baked into the minted id and
        /// mixed into the topic.
        config: SwarmConfig,
        /// The directory this swarm advertises into, if any. Drives the
        /// `advertising on #<directory>` startup line; the re-broadcast
        /// task itself is spawned by the caller post-setup.
        advertise: Option<SwarmName>,
    },
    Join {
        swarm: Swarm,
    },
    /// A `forum` swarm derived from a shared string. Identical to `Join`
    /// except the first peer must beacon: there is no distinguished creator,
    /// so it co-hosts the shared rendezvous eagerly-but-probed
    /// ([`CoHostPolicy::EagerProbed`]) rather than deferring.
    Forum {
        swarm: Swarm,
    },
}

/// Build the `RendezvousParams` for a swarm. `id` is the well-known
/// rendezvous `EndpointId`; `bind_port` is `Some` only for private
/// swarms (the deterministic loopback port — public is ephemeral and
/// pkarr-discoverable).
fn rendezvous_params(
    swarm: &Swarm,
    topic_id: iroh_gossip::proto::TopicId,
    lookups: &LookupOpts,
    rung_tx: watch::Sender<Option<RelayUrl>>,
) -> RendezvousParams {
    let bind_ports = if swarm.is_loopback() {
        swarm.rendezvous_ports().to_vec()
    } else {
        Vec::new()
    };
    // Optimistic rung 0 — the first ladder rung, **unprobed**, so setup
    // (and the joiner's `ready`) never blocks on a relay handshake. A
    // backgrounded probe (`spawn_startup_rung_confirmation`) and the
    // beacon's own liveness self-monitor correct it off the event loop
    // via `rung_tx` if rung 0 turns out to be unreachable. Empty for
    // private / relay-disabled ⇒ `None`.
    let bootstrap_relay = relay_ladder(&lookups.relay).first().cloned();
    RendezvousParams {
        topic_id,
        secret: rendezvous_secret(swarm.seed()),
        bind_ports,
        id: swarm.rendezvous_id(),
        lookups: lookups.clone(),
        bootstrap_relay,
        rung_tx,
    }
}

/// Confirm the optimistic rung 0 **off the event loop**: walk the ladder
/// once (the only relay handshakes setup pays, and detached so `ready`
/// is already out) and, if the first *reachable* rung differs from rung
/// 0, publish it through `rung_tx`. Covers a rung-0-down-at-start for
/// both creator and joiner; the joiner has no beacon self-monitor, so
/// this is its only startup correction. No-op for an empty (private /
/// relay-disabled) ladder.
fn spawn_startup_rung_confirmation(
    ladder: Vec<RelayUrl>,
    rung_tx: watch::Sender<Option<RelayUrl>>,
) {
    if ladder.is_empty() {
        return;
    }
    tokio::spawn(async move {
        let confirmed =
            select_bootstrap_rung(&ladder, Duration::from_secs(RELAY_RUNG_PROBE_SECS)).await;
        rung_tx.send_if_modified(|current| {
            if *current == confirmed {
                false
            } else {
                *current = confirmed;
                true
            }
        });
    });
}

/// Pre-register `rendezvous_id`'s address so a cold joiner reaches it
/// with **zero address-lookup wait** — a creator-independent bootstrap
/// that needs no discovery query to make the first dial:
///
/// - **loopback-only**: every loopback ladder rung (iroh's node-id dial
///   reaches whichever rung our beacon bound; see `crate::beacon`).
/// - **reachable across machines**: the seed-derived `rendezvous_id` at
///   the chosen relay **rung** (`params.bootstrap_relay` — the first
///   reachable rung of the ladder) — the beacon homes there
///   (`beacon::beacon_lookups`), so this is a real relay-direct dial, no
///   DNS/DHT/mDNS round-trip. DHT/mDNS stay wired as the eternal/LAN
///   backstop if every rung is ever unreachable.
///
/// Also re-asserted by the daemon's hard-heal path (resume / silent
/// partition): the startup hint may now point at a dead path, so the
/// relay-homed `rendezvous_id` address is registered again before the
/// long re-bootstrap probe.
pub(crate) fn register_rendezvous(endpoint: &Endpoint, params: &RendezvousParams) {
    let mut addr = EndpointAddr::new(params.id);
    if !params.bind_ports.is_empty() {
        for &port in &params.bind_ports {
            addr = addr.with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
        }
        // Explicit target: needs RendezvousParams, so can't live in lookups.
        tracing::info!(
            target: "agent_habilis_swarm::lookup",
            rungs = params.bind_ports.len(),
            "pre-registered rendezvous on the loopback port ladder"
        );
    } else if let Some(relay) = params.bootstrap_relay.clone() {
        tracing::info!(
            target: "agent_habilis_swarm::lookup",
            relay = %relay,
            "pre-registered rendezvous at the relay rung for zero-lookup dial"
        );
        addr = addr.with_relay_url(relay);
    } else {
        // Relay disabled (not in the allowlist) or private without a
        // port ladder: nothing to pre-register — joiners resolve the
        // rendezvous id via mDNS/DHT only.
        return;
    }
    let _ = add_peer_addr(endpoint, addr);
}

/// Build the endpoint, subscribe to the topic, and produce a ready
/// `EventLoopConfig`. Shared by the `create` / `join` CLI paths and
/// the embed + MCP sessions.
///
/// Output ordering differs by design: `Create` prints the swarm ID to
/// stderr, then `info`, then `ready`; `Join` emits `ready` then `info`.
pub(crate) async fn setup_swarm(
    kind: SetupKind,
    author: Nickname,
    interactive: bool,
    max_peers: usize,
    state_file: Option<PathBuf>,
    output: output::Output,
    // Skill-drift warning folded into the `ready` event. Computed by the CLI
    // (the real `ahsw create`/`join` path) from the on-disk install; `None` on
    // the embed/library and MCP paths, which keeps the in-process tests
    // hermetic (no dependence on the dev machine's install state).
    drift: Option<&str>,
) -> Result<EventLoopConfig> {
    // Create mints the config from the caller's choices; join decodes it
    // from the id — one source of truth either way.
    let lookups = match &kind {
        SetupKind::Create { config, .. } => config.lookups.clone(),
        SetupKind::Join { swarm } | SetupKind::Forum { swarm } => swarm.lookups().clone(),
    };

    // The off-loop rung channel: the backgrounded startup probe and the
    // beacon's liveness self-monitor publish a chosen rung here; the
    // event loop applies it (re-register + re-home) without ever running
    // a ladder walk on the sole loop. Initialized to the optimistic
    // rung 0 (empty ladder ⇒ `None`).
    let ladder = relay_ladder(&lookups.relay);
    let (rung_tx, rung_rx) = watch::channel(ladder.first().cloned());

    let (swarm_id, swarm_name, endpoint, router, gossip, topic, rdv, cohost) = match kind {
        SetupKind::Create {
            name,
            config,
            advertise,
        } => {
            let mut seed = [0u8; 32];
            rand::rng().fill_bytes(&mut seed);

            let endpoint = build_participant_endpoint(&lookups).await?;

            let swarm = Swarm::new(seed, name.clone(), config);
            let id_str = swarm.to_string();
            let swarm_id = SwarmId::new(id_str.clone())
                .expect("Swarm::to_string always produces a valid SwarmId");

            output.info(&format!("created #{name} and joined as <{author}>"));
            if let Some(directory) = &advertise {
                output.info(&format!("advertising on #{directory}"));
            }
            output.swarm_id_line(&swarm_id);
            output.ready(&swarm_id, &name, &author, drift);
            lifecycle::log_ready(
                &id_str,
                name.as_str(),
                author.as_str(),
                swarm.network_label(),
            );

            let topic_id = derive_topic_id(swarm.seed(), &swarm.name, &swarm.config_bytes());
            let (gossip, router) = build_swarm(endpoint.clone());
            // Creator has no peers yet — bootstrap is empty.
            let topic = gossip.subscribe(topic_id, vec![]).await?;

            let rdv = rendezvous_params(&swarm, topic_id, &lookups, rung_tx.clone());
            register_rendezvous(&endpoint, &rdv);

            // The origin co-hosts the rendezvous immediately: it has
            // no bootstrap dial to self-collide with, and an otherwise
            // empty swarm needs a beacon from t=0.
            (
                swarm_id,
                name,
                endpoint,
                router,
                gossip,
                topic,
                rdv,
                CoHostPolicy::Eager,
            )
        }
        kind @ (SetupKind::Join { .. } | SetupKind::Forum { .. }) => {
            // Join and Forum share one attach path; they differ only in the
            // co-host policy (Forum has no distinguished creator, so its first
            // peer must beacon) and the startup verb.
            let (swarm, cohost, verb) = match kind {
                SetupKind::Join { swarm } => (swarm, CoHostPolicy::Deferred, "joined"),
                SetupKind::Forum { swarm } => (swarm, CoHostPolicy::EagerProbed, "joined forum"),
                SetupKind::Create { .. } => unreachable!("outer arm excludes Create"),
            };
            let id_str = swarm.to_string();
            let swarm_id = SwarmId::new(id_str.clone())
                .expect("Swarm::to_string always produces a valid SwarmId");
            let topic_id = derive_topic_id(swarm.seed(), &swarm.name, &swarm.config_bytes());

            let endpoint = build_participant_endpoint(&lookups).await?;

            let rdv = rendezvous_params(&swarm, topic_id, &lookups, rung_tx.clone());
            // Must precede the join: the participant resolves the
            // rendezvous id via this registered address — the loopback
            // port ladder (loopback-only) or the chosen relay rung
            // (reachable across machines).
            register_rendezvous(&endpoint, &rdv);

            let (gossip, router) = build_swarm(endpoint.clone());
            // Non-blocking, like `create`: `ready` fires immediately so
            // the joiner is never invisible while bootstrapping, and an
            // empty swarm (everyone left) is still joinable. We
            // subscribe, background-connect to the rendezvous, and — for a
            // plain join — `daemon::run` defers co-hosting our own (same
            // seed-id) rendezvous until we are meshed, so we never register a
            // duplicate `rendezvous_id` on the shared pinned relay that could
            // capture our own bootstrap dial. A forum instead claims eagerly
            // (probe-first) so the first peer beacons. See
            // `EventLoopConfig::cohost`.
            let topic = gossip.subscribe(topic_id, vec![rdv.id]).await?;

            output.ready(&swarm_id, &swarm.name, &author, drift);
            lifecycle::log_ready(
                &id_str,
                swarm.name.as_str(),
                author.as_str(),
                swarm.network_label(),
            );
            output.info(&format!("{verb} #{} as <{author}>", swarm.name));

            (
                swarm_id, swarm.name, endpoint, router, gossip, topic, rdv, cohost,
            )
        }
    };

    // Off the critical path: `ready` is already out. Confirm/correct the
    // optimistic rung 0 in the background (covers a joiner, which has no
    // beacon self-monitor of its own).
    spawn_startup_rung_confirmation(ladder, rung_tx);

    // This member's per-author signing identity. In-process / ephemeral:
    // minted here, held for the process lifetime, never persisted (a
    // restart is a fresh identity). See `crate::protocol::identity`.
    let identity = std::sync::Arc::new(crate::protocol::identity::Identity::generate());

    Ok(EventLoopConfig {
        topic,
        gossip,
        author,
        identity,
        swarm: swarm_id,
        name: swarm_name,
        output,
        interactive,
        endpoint,
        router,
        max_peers,
        rendezvous_params: rdv,
        rung_rx,
        cohost,
        state_file,
        // Set by the advertise path (cli::create / embed::create) before
        // `run`; absent for every non-advertising session.
        live_count: None,
        // Default to the CLI driver; the MCP / embed sessions
        // overwrite `cfg.driver` before handing it to `daemon::run`.
        driver: DriverMode::Cli,
    })
}
