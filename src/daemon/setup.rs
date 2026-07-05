use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use iroh::{Endpoint, EndpointAddr, RelayUrl};
use rand::RngCore;
use tokio::sync::{mpsc, watch};

use crate::lookup::{
    add_peer_addr, build_participant_endpoint, build_swarm, relay_ladder, select_bootstrap_rung,
};
use crate::output;
use crate::protocol::crypto::Password;
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
        /// Password to protect the swarm with. Stretched *here* rather than
        /// by the frontend: the verifier's salt is the seed, which is
        /// minted below.
        password: Option<Password>,
    },
    Join {
        swarm: Swarm,
        /// The password the joiner presented (already verified + applied to
        /// `swarm`), retained so the daemon can key blob-ticket protection with
        /// it. `None` for a passwordless id.
        password: Option<Password>,
    },
    /// A `topic` swarm derived from a shared string. Identical to `Join`
    /// except the first peer must beacon: there is no distinguished creator,
    /// so it co-hosts the shared rendezvous eagerly-but-probed
    /// ([`CoHostPolicy::EagerProbed`]) rather than deferring.
    Topic { swarm: Swarm },
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
        secret: swarm.rendezvous_secret(),
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
            target: "agent_gossip::lookup",
            rungs = params.bind_ports.len(),
            "pre-registered rendezvous on the loopback port ladder"
        );
    } else if let Some(relay) = params.bootstrap_relay.clone() {
        tracing::info!(
            target: "agent_gossip::lookup",
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
/// The unicast inbound channel + its Router acceptor. The acceptor forwards
/// received `UNICAST_ALPN` frames to the returned receiver, which the event
/// loop drains into `gossip::ingest`. Bounded so a flooding peer can't
/// back-pressure the loop (a dropped frame heals via anti-entropy).
fn unicast_inbox() -> (
    mpsc::Receiver<bytes::Bytes>,
    crate::unicast::UnicastAcceptor,
    mpsc::Sender<bytes::Bytes>,
) {
    let (tx, rx) = mpsc::channel::<bytes::Bytes>(crate::util::consts::UNICAST_INBOX_CAP);
    // The relay's terminal delivery shares this inbox, so a relayed frame lands
    // in the same `gossip::ingest` path as a unicast one.
    (rx, crate::unicast::UnicastAcceptor::new(tx.clone()), tx)
}

#[expect(
    clippy::too_many_arguments,
    reason = "the one-shot session assembly: every argument is a distinct per-session input (identity, io, tuning, bindings) with no meaningful grouping"
)]
#[expect(
    clippy::too_many_lines,
    reason = "the one-shot session assembly: a Create/Join match whose arms each build endpoint + swarm + rendezvous; splitting it would thread a dozen locals through a helper for no clarity gain"
)]
pub(crate) async fn setup_swarm(
    kind: SetupKind,
    author: Nickname,
    interactive: bool,
    max_peers: usize,
    state_file: Option<PathBuf>,
    output: output::Output,
    // Skill-drift warning folded into the `ready` event. Computed by the CLI
    // (the real `agent-gossip create`/`join` path) from the on-disk install; `None` on
    // the embed/library and MCP paths, which keeps the in-process tests
    // hermetic (no dependence on the dev machine's install state).
    drift: Option<&str>,
    // `--a2a-serve`: bind the localhost JSON-RPC binding on this port
    // (`0` = OS-assigned) — bound here, before `ready` fires, so the event
    // carries the real port. `None` (embed/MCP and the flag's default)
    // serves nothing.
    a2a_serve: Option<u16>,
) -> Result<EventLoopConfig> {
    let a2a = match a2a_serve {
        Some(port) => Some(crate::a2a::http::bind(port).await?),
        None => None,
    };
    let a2a_port = a2a.as_ref().map(|binding| binding.port);
    // Create mints the config from the caller's choices; join decodes it
    // from the id — one source of truth either way.
    let lookups = match &kind {
        SetupKind::Create { config, .. } => config.lookups.clone(),
        SetupKind::Join { swarm, .. } | SetupKind::Topic { swarm } => swarm.lookups().clone(),
    };

    // The off-loop rung channel: the backgrounded startup probe and the
    // beacon's liveness self-monitor publish a chosen rung here; the
    // event loop applies it (re-register + re-home) without ever running
    // a ladder walk on the sole loop. Initialized to the optimistic
    // rung 0 (empty ladder ⇒ `None`).
    let ladder = relay_ladder(&lookups.relay);
    let (rung_tx, rung_rx) = watch::channel(ladder.first().cloned());

    let (unicast_rx, unicast_acceptor, inbox_tx) = unicast_inbox();

    // This member's per-author signing identity (also the source of its X25519
    // seal key, which relays peel circuit onions with). Hoisted above the match
    // so the relay acceptor can be built before the Router is spawned.
    let identity = std::sync::Arc::new(crate::protocol::identity::Identity::generate());
    // The relay acceptor needs the participant endpoint to dial the next hop, but
    // is registered on the Router *before* that endpoint is bound below; it reads
    // the endpoint from this cell, filled once the endpoint exists.
    let whisper_endpoint: std::sync::Arc<std::sync::OnceLock<Endpoint>> =
        std::sync::Arc::new(std::sync::OnceLock::new());
    let whisper_acceptor = crate::whisper::WhisperAcceptor::new(
        inbox_tx,
        identity.seal_secret(),
        whisper_endpoint.clone(),
    );

    #[rustfmt::skip]
    let (swarm_id, swarm_name, endpoint, router, gossip, topic, rdv, cohost, swarm_password, swarm_key) = match kind {
        SetupKind::Create {
            name,
            config,
            advertise,
            password,
        } => {
            let mut seed = [0u8; 32];
            rand::rng().fill_bytes(&mut seed);

            let endpoint = build_participant_endpoint(&lookups).await?;

            let swarm = Swarm::new(seed, name.clone(), config);
            // Bake the verifier into the config BEFORE the id is rendered
            // (the id must carry it) and before any derivation. The
            // ~100ms Argon2id stretch runs off the async worker. The password
            // is returned from the worker (not dropped) so the daemon can key
            // blob tickets with it.
            let (swarm, swarm_password) = match password {
                Some(password) => {
                    tokio::task::spawn_blocking(move || {
                        let mut swarm = swarm;
                        swarm.set_password(&password);
                        (swarm, Some(password))
                    })
                    .await?
                }
                None => (swarm, None),
            };
            let swarm_key = swarm.stretched_key().map(zeroize::Zeroizing::new);
            let id_str = swarm.to_string();
            let swarm_id = SwarmId::new(id_str.clone())
                .expect("Swarm::to_string always produces a valid SwarmId");

            output.info(&format!("created #{name} and joined as <{author}>"));
            if swarm.requires_password() {
                output.info("password-protected — joiners must present the password");
            }
            if let Some(directory) = &advertise {
                output.info(&format!("advertising on #{directory}"));
            }
            output.swarm_id_line(&swarm_id);
            output.ready(&swarm_id, &name, &author, drift, a2a_port);
            lifecycle::log_ready(
                &id_str,
                name.as_str(),
                author.as_str(),
                swarm.network_label(),
            );

            let topic_id = swarm.topic_id();
            let (gossip, router) = build_swarm(
                endpoint.clone(),
                max_peers,
                Some(unicast_acceptor.clone()),
                Some(whisper_acceptor.clone()),
            );
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
                swarm_password,
                swarm_key,
            )
        }
        kind @ (SetupKind::Join { .. } | SetupKind::Topic { .. }) => {
            // Join and Topic share one attach path; they differ only in the
            // co-host policy (Topic has no distinguished creator, so its first
            // peer must beacon) and the startup verb.
            let (swarm, swarm_password, cohost, verb) = match kind {
                SetupKind::Join { swarm, password } => {
                    (swarm, password, CoHostPolicy::Deferred, "joined")
                }
                // A topic swarm is always passwordless.
                SetupKind::Topic { swarm } => (swarm, None, CoHostPolicy::EagerProbed, "joined topic"),
                SetupKind::Create { .. } => unreachable!("outer arm excludes Create"),
            };
            let swarm_key = swarm.stretched_key().map(zeroize::Zeroizing::new);
            let id_str = swarm.to_string();
            let swarm_id = SwarmId::new(id_str.clone())
                .expect("Swarm::to_string always produces a valid SwarmId");
            let topic_id = swarm.topic_id();

            let endpoint = build_participant_endpoint(&lookups).await?;

            let rdv = rendezvous_params(&swarm, topic_id, &lookups, rung_tx.clone());
            // Must precede the join: the participant resolves the
            // rendezvous id via this registered address — the loopback
            // port ladder (loopback-only) or the chosen relay rung
            // (reachable across machines).
            register_rendezvous(&endpoint, &rdv);

            let (gossip, router) = build_swarm(
                endpoint.clone(),
                max_peers,
                Some(unicast_acceptor.clone()),
                Some(whisper_acceptor.clone()),
            );
            // Non-blocking, like `create`: `ready` fires immediately so
            // the joiner is never invisible while bootstrapping, and an
            // empty swarm (everyone left) is still joinable. We
            // subscribe, background-connect to the rendezvous, and — for a
            // plain join — `daemon::run` defers co-hosting our own (same
            // seed-id) rendezvous until we are meshed, so we never register a
            // duplicate `rendezvous_id` on the shared pinned relay that could
            // capture our own bootstrap dial. A topic instead claims eagerly
            // (probe-first) so the first peer beacons. See
            // `EventLoopConfig::cohost`.
            let topic = gossip.subscribe(topic_id, vec![rdv.id]).await?;

            output.ready(&swarm_id, &swarm.name, &author, drift, a2a_port);
            lifecycle::log_ready(
                &id_str,
                swarm.name.as_str(),
                author.as_str(),
                swarm.network_label(),
            );
            output.info(&format!("{verb} #{} as <{author}>", swarm.name));

            (
                swarm_id,
                swarm.name,
                endpoint,
                router,
                gossip,
                topic,
                rdv,
                cohost,
                swarm_password,
                swarm_key,
            )
        }
    };

    // Now that the endpoint is bound, hand it to the relay acceptor so it can
    // dial the next hop when forwarding a circuit (`set` is a no-op if the
    // acceptor was never registered — the beacon/rendezvous path).
    let _ = whisper_endpoint.set(endpoint.clone());

    // Off the critical path: `ready` is already out. Confirm/correct the
    // optimistic rung 0 in the background (covers a joiner, which has no
    // beacon self-monitor of its own).
    spawn_startup_rung_confirmation(ladder, rung_tx);

    Ok(EventLoopConfig {
        topic,
        gossip,
        author,
        identity,
        swarm: swarm_id,
        name: swarm_name,
        swarm_password,
        swarm_key,
        output,
        interactive,
        endpoint,
        router,
        max_peers,
        rendezvous_params: rdv,
        rung_rx,
        cohost,
        state_file,
        unicast_rx,
        a2a,
        // Set by the advertise path (cli::create / embed::create) before
        // `run`; absent for every non-advertising session.
        live_count: None,
        // Default to the CLI driver; the MCP / embed sessions
        // overwrite `cfg.driver` before handing it to `daemon::run`.
        driver: DriverMode::Cli,
    })
}
