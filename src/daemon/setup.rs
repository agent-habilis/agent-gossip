use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use anyhow::Result;
use iroh::{Endpoint, EndpointAddr};
use rand::RngCore;

use crate::lookup::{add_peer_addr, build_participant_endpoint, build_swarm, relay_url};
use crate::output;
use crate::protocol::crypto::{derive_topic_id, rendezvous_secret};
use crate::protocol::swarm::{LookupOpts, Swarm, SwarmMode, SwarmName};
use crate::protocol::{Nickname, SwarmId};

use crate::beacon::RendezvousParams;
use crate::lifecycle;

use super::{CoHostPolicy, DriverMode, EventLoopConfig};

/// What kind of swarm we're setting up — either minting a new one
/// (create) or attaching to an existing one (join).
pub(crate) enum SetupKind {
    Create { mode: SwarmMode, name: SwarmName },
    Join { swarm: Swarm },
}

/// Build the `RendezvousParams` for a swarm. `id` is the well-known
/// rendezvous `EndpointId`; `bind_port` is `Some` only for private
/// swarms (the deterministic loopback port — public is ephemeral and
/// pkarr-discoverable).
fn rendezvous_params(
    swarm: &Swarm,
    topic_id: iroh_gossip::proto::TopicId,
    lookups: &LookupOpts,
) -> RendezvousParams {
    let bind_ports = if swarm.mode == SwarmMode::Private {
        swarm.rendezvous_ports().to_vec()
    } else {
        Vec::new()
    };
    RendezvousParams {
        mode: swarm.mode,
        topic_id,
        secret: rendezvous_secret(swarm.seed()),
        bind_ports,
        id: swarm.rendezvous_id(),
        lookups: lookups.clone(),
    }
}

/// Pre-register `rendezvous_id`'s address so a cold joiner reaches it
/// with **zero address-lookup wait** — the creator-independent analog
/// of the pre-rewrite ticket's embedded address (the path that made
/// public lookup instant):
///
/// - **private**: every loopback ladder rung (iroh's node-id dial
///   reaches whichever rung our beacon bound; see `crate::beacon`).
/// - **public**: the seed-derived `rendezvous_id` at the single
///   pinned (or `--relay`) relay — the beacon homes there
///   (`beacon::beacon_lookups`), so this is a real relay-direct
///   dial, no DNS/DHT/mDNS round-trip. DHT/mDNS stay wired as the
///   eternal/LAN backstop if this relay is ever unreachable.
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
    } else if let Some(relay) = relay_url(&params.lookups.relay) {
        tracing::info!(
            target: "agent_habilis_swarm::lookup",
            relay = %relay,
            "pre-registered rendezvous at the relay for zero-lookup dial"
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
    lookups: LookupOpts,
    output: output::Output,
) -> Result<EventLoopConfig> {
    let (swarm_id, swarm_name, endpoint, router, topic, rdv, cohost) = match kind {
        SetupKind::Create { mode, name } => {
            let mut seed = [0u8; 32];
            rand::rng().fill_bytes(&mut seed);

            let endpoint = build_participant_endpoint(mode, &lookups).await?;

            let swarm = Swarm::new(mode, seed, name.clone());
            let id_str = swarm.to_string();
            let swarm_id = SwarmId::new(id_str.clone())
                .expect("Swarm::to_string always produces a valid SwarmId");

            output.info(&format!("created #{name} and joined as <{author}>"));
            output.swarm_id_line(&id_str);
            output.ready(&id_str, name.as_str(), author.as_str());
            lifecycle::log_ready(&id_str, name.as_str(), author.as_str(), mode.network_name());

            let topic_id = derive_topic_id(&seed, &name);
            let (gossip, router) = build_swarm(endpoint.clone());
            // Creator has no peers yet — bootstrap is empty.
            let topic = gossip.subscribe(topic_id, vec![]).await?;

            let rdv = rendezvous_params(&swarm, topic_id, &lookups);
            register_rendezvous(&endpoint, &rdv);

            // The origin co-hosts the rendezvous immediately: it has
            // no bootstrap dial to self-collide with, and an otherwise
            // empty swarm needs a beacon from t=0.
            (
                swarm_id,
                name,
                endpoint,
                router,
                topic,
                rdv,
                CoHostPolicy::Eager,
            )
        }
        SetupKind::Join { swarm } => {
            let id_str = swarm.to_string();
            let swarm_id = SwarmId::new(id_str.clone())
                .expect("Swarm::to_string always produces a valid SwarmId");
            let topic_id = derive_topic_id(swarm.seed(), &swarm.name);

            let endpoint = build_participant_endpoint(swarm.mode, &lookups).await?;

            let rdv = rendezvous_params(&swarm, topic_id, &lookups);
            // Must precede the join: the participant resolves the
            // rendezvous id via this registered address — the loopback
            // ladder (private) or the pinned relay (public).
            register_rendezvous(&endpoint, &rdv);

            let (gossip, router) = build_swarm(endpoint.clone());
            // Non-blocking, like `create`: `ready` fires immediately so
            // the joiner is never invisible while bootstrapping, and an
            // empty swarm (everyone left) is still joinable. We
            // subscribe, background-connect to the rendezvous, and
            // `daemon::run` defers co-hosting our own (same seed-id)
            // rendezvous until we are meshed — so we never register a
            // duplicate `rendezvous_id` on the shared pinned relay that
            // could capture our own bootstrap dial. See
            // `EventLoopConfig::cohost`.
            let topic = gossip.subscribe(topic_id, vec![rdv.id]).await?;

            output.ready(&id_str, swarm.name.as_str(), author.as_str());
            lifecycle::log_ready(
                &id_str,
                swarm.name.as_str(),
                author.as_str(),
                swarm.mode.network_name(),
            );
            output.info(&format!("joined #{} as <{author}>", swarm.name));

            (
                swarm_id,
                swarm.name,
                endpoint,
                router,
                topic,
                rdv,
                CoHostPolicy::Deferred,
            )
        }
    };

    Ok(EventLoopConfig {
        topic,
        author,
        swarm: swarm_id,
        name: swarm_name,
        output,
        interactive,
        endpoint,
        router,
        max_peers,
        rendezvous_params: rdv,
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
