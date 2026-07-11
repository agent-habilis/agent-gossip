use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use iroh::{Endpoint, EndpointAddr, RelayUrl};
use rand::RngCore;
use tokio::sync::{mpsc, watch};

use crate::gossip::event::{NodeEvent, NodeSink};
use crate::lookup::{
    add_peer_addr, build_mesh, build_participant_endpoint, build_participant_multihop,
    relay_ladder, select_bootstrap_rung,
};
use crate::protocol::crypto::Password;
use crate::protocol::mesh::{LookupOpts, Mesh, MeshConfig, MeshName};
use crate::protocol::{MeshId, Nickname};
use crate::util::tuning::RELAY_RUNG_PROBE_SECS;

use crate::beacon::RendezvousParams;
use crate::lifecycle;

use super::{CoHostPolicy, DriverMode, EventLoopConfig, ReadyAnnounce};

/// What kind of mesh we're setting up — either minting a new one
/// (create) or attaching to an existing one (join).
#[derive(Debug)]
pub enum SetupKind {
    Create {
        name: MeshName,
        /// The mesh-wide config (lookups) baked into the minted id and
        /// mixed into the topic.
        config: MeshConfig,
        /// The directory this mesh advertises into, if any. Drives the
        /// `advertising on #<directory>` startup line; the re-broadcast
        /// task itself is spawned by the caller post-setup.
        advertise: Option<MeshName>,
        /// Password to protect the mesh with. Stretched *here* rather than
        /// by the frontend: the verifier's salt is the seed, which is
        /// minted below.
        password: Option<Password>,
        /// `--invite-only`: mint an invite root + issuer keypair here (the
        /// issuer pubkey is baked into the id) so the bare hash can't join —
        /// only creator-minted `🎟️` invites can.
        invite_only: bool,
    },
    Join {
        mesh: Mesh,
        /// The password the joiner presented (already verified + applied to
        /// `mesh`), retained so the daemon can key blob-ticket protection with
        /// it. `None` for a passwordless id.
        password: Option<Password>,
    },
    /// A `topic` mesh derived from a shared string. Identical to `Join`
    /// except the first peer must beacon: there is no distinguished creator,
    /// so it co-hosts the shared rendezvous eagerly-but-probed
    /// ([`CoHostPolicy::EagerProbed`]) rather than deferring.
    Topic { mesh: Mesh },
}

/// Build the `RendezvousParams` for a mesh. `id` is the well-known
/// rendezvous `EndpointId`; `bind_port` is `Some` only for private
/// meshes (the deterministic loopback port — public is ephemeral and
/// pkarr-discoverable).
fn rendezvous_params(
    mesh: &Mesh,
    topic_id: iroh_gossip::proto::TopicId,
    lookups: &LookupOpts,
    rung_tx: watch::Sender<Option<RelayUrl>>,
) -> RendezvousParams {
    let bind_ports = if mesh.is_loopback() {
        mesh.rendezvous_ports().to_vec()
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
        secret: mesh.rendezvous_secret(),
        bind_ports,
        id: mesh.rendezvous_id(),
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
            target: "agent_square::lookup",
            rungs = params.bind_ports.len(),
            "pre-registered rendezvous on the loopback port ladder"
        );
    } else if let Some(relay) = params.bootstrap_relay.clone() {
        tracing::info!(
            target: "agent_square::lookup",
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
/// Output ordering differs by design: `Create` prints the mesh ID to
/// stderr, then `info`, then `ready`; `Join` emits `ready` then `info`.
/// The unicast inbound channel + its Router acceptor. The acceptor forwards
/// received `UNICAST_ALPN` frames to the returned receiver, which the event
/// loop drains into `gossip::ingest`. Bounded so a flooding peer can't
/// back-pressure the loop (a dropped frame heals via anti-entropy).
fn unicast_inbox() -> (
    mpsc::Receiver<bytes::Bytes>,
    crate::unicast::UnicastAcceptor,
) {
    let (tx, rx) = mpsc::channel::<bytes::Bytes>(crate::util::consts::UNICAST_INBOX_CAP);
    (rx, crate::unicast::UnicastAcceptor::new(tx))
}

/// Build this member's participant endpoint, registering the multi-hop transport
/// (and standing up its underlay) when `--multihop` is set.
async fn build_member_endpoint(
    build: &SetupBuild<'_>,
) -> Result<(Endpoint, Option<iroh_multihop_transport::MultihopHandle>)> {
    if build.multihop {
        let (endpoint, handle) = build_participant_multihop(build.lookups).await?;
        Ok((endpoint, Some(handle)))
    } else {
        Ok((build_participant_endpoint(build.lookups).await?, None))
    }
}

/// The per-session inputs to [`setup_mesh`] that are independent of the
/// [`SetupKind`]: this member's identity/io/tuning and the localhost
/// binding request. Bundled so the one-shot assembly stays within the
/// argument budget.
pub struct SetupParams<'a> {
    pub author: Nickname,
    pub max_peers: usize,
    pub state_file: Option<PathBuf>,
    pub sink: std::sync::Arc<dyn NodeSink>,
    /// Which transports directed messages may use (per-session). Defaults to
    /// [`crate::transport::TransportPolicy::DEFAULTS`] (all enabled) on the
    /// embed/MCP paths; the CLI derives it from `--no-unicast`/`--no-*`.
    pub transport: crate::transport::TransportPolicy,
    /// `--multihop`: register the multi-hop custom transport on the participant
    /// endpoint (a second underlay endpoint is stood up for hop-by-hop
    /// forwarding). Off by default on every path.
    pub multihop: bool,
    /// Skill-drift warning folded into the `ready` event. Computed by the CLI
    /// (the real `agent-square create`/`join` path) from the on-disk install;
    /// `None` on the embed/library and MCP paths, which keeps the in-process
    /// tests hermetic (no dependence on the dev machine's install state).
    pub drift: Option<&'a str>,
    /// `--a2a-serve`: bind the localhost JSON-RPC binding on this port
    /// (`0` = OS-assigned) — bound here, before `ready` fires, so the event
    /// carries the real port. `None` (embed/MCP and the flag's default)
    /// serves nothing.
    pub a2a_serve: Option<u16>,
}

impl std::fmt::Debug for SetupParams<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetupParams").finish_non_exhaustive()
    }
}

/// The kind-independent build inputs threaded into [`setup_create`] /
/// [`setup_join`]: the loop-shared refs both attach paths need to stand up
/// the endpoint, gossip overlay and rendezvous.
struct SetupBuild<'a> {
    author: &'a Nickname,
    sink: &'a dyn NodeSink,
    drift: Option<&'a str>,
    a2a_port: Option<u16>,
    max_peers: usize,
    lookups: &'a LookupOpts,
    unicast_acceptor: &'a crate::unicast::UnicastAcceptor,
    /// Register the multi-hop transport on the participant endpoint.
    multihop: bool,
    rung_tx: &'a watch::Sender<Option<RelayUrl>>,
}

/// The freshly-assembled mesh handles a [`SetupKind`] arm produces.
struct Assembled {
    mesh_id: MeshId,
    mesh_name: MeshName,
    endpoint: Endpoint,
    router: iroh::protocol::Router,
    gossip: iroh_gossip::net::Gossip,
    topic: iroh_gossip::api::GossipTopic,
    rdv: RendezvousParams,
    cohost: CoHostPolicy,
    /// The raw mesh password (passworded mesh) — retained so the daemon can
    /// key blob tickets with it. `None` when passwordless.
    mesh_password: Option<Password>,
    /// The Argon2id-stretched mesh key — retained to derive the state/meta/
    /// broadcast content-encryption keys. `None` when passwordless. Wiped on drop.
    mesh_key: Option<zeroize::Zeroizing<[u8; 32]>>,
    /// The invite-only creator's mesh (with its in-memory issuer key + root),
    /// retained so `invite` can mint. `Some` only on an invite-only creator.
    mint_mesh: Option<Mesh>,
    /// The multi-hop transport handle when `--multihop` registered it, threaded
    /// into `EventLoopState` so the link-state tick can feed its routing table.
    multihop: Option<iroh_multihop_transport::MultihopHandle>,
}

/// # Errors
/// Returns an error if the inputs are invalid or the operation fails.
pub async fn setup_mesh(kind: SetupKind, params: SetupParams<'_>) -> Result<EventLoopConfig> {
    let SetupParams {
        author,
        max_peers,
        state_file,
        sink,
        transport,
        multihop,
        drift,
        a2a_serve,
    } = params;
    // The a2a localhost binding is owned + bound by the CLI caller (which holds
    // `A2aApp`); the engine only needs the resolved port for the `ready` event
    // and the published card. `Some(0)` (ephemeral) is resolved caller-side and
    // passed back in here as the real port.
    let a2a_port = a2a_serve;
    // Create mints the config from the caller's choices; join decodes it
    // from the id — one source of truth either way.
    let lookups = match &kind {
        SetupKind::Create { config, .. } => config.lookups.clone(),
        SetupKind::Join { mesh, .. } | SetupKind::Topic { mesh } => mesh.lookups().clone(),
    };

    // The off-loop rung channel: the backgrounded startup probe and the
    // beacon's liveness self-monitor publish a chosen rung here; the
    // event loop applies it (re-register + re-home) without ever running
    // a ladder walk on the sole loop. Initialized to the optimistic
    // rung 0 (empty ladder ⇒ `None`).
    let ladder = relay_ladder(&lookups.relay);
    let (rung_tx, rung_rx) = watch::channel(ladder.first().cloned());

    let (unicast_rx, unicast_acceptor) = unicast_inbox();

    // This member's per-author signing identity. Hoisted above the match so it is
    // available to both attach paths.
    let identity = std::sync::Arc::new(crate::protocol::identity::Identity::generate());

    let build = SetupBuild {
        author: &author,
        sink: sink.as_ref(),
        drift,
        a2a_port,
        max_peers,
        lookups: &lookups,
        unicast_acceptor: &unicast_acceptor,
        multihop,
        rung_tx: &rung_tx,
    };
    let Assembled {
        mesh_id,
        mesh_name,
        endpoint,
        router,
        gossip,
        topic,
        rdv,
        cohost,
        mesh_password,
        mesh_key,
        mint_mesh,
        multihop: multihop_handle,
    } = match kind {
        SetupKind::Create {
            name,
            config,
            advertise,
            password,
            invite_only,
        } => {
            setup_create(
                &build,
                CreateSetup {
                    name,
                    config,
                    advertise,
                    password,
                    invite_only,
                },
            )
            .await?
        }
        kind @ (SetupKind::Join { .. } | SetupKind::Topic { .. }) => {
            setup_join(&build, kind).await?
        }
    };

    // Read out of `build` before anything below moves what it borrows
    // (`author`, `sink`, `rung_tx`).
    let ready = ReadyAnnounce {
        drift: build.drift.map(str::to_owned),
        a2a_port: build.a2a_port,
    };

    // Off the critical path: nothing below blocks `ready`, which `run` emits
    // once the IPC socket accepts. Confirm/correct the optimistic rung 0 in the
    // background (covers a joiner, which has no beacon self-monitor of its own).
    spawn_startup_rung_confirmation(ladder, rung_tx);

    Ok(EventLoopConfig {
        topic,
        gossip,
        author,
        identity,
        mesh: mesh_id,
        name: mesh_name,
        mesh_password,
        mesh_key,
        sink,
        mint_mesh,
        endpoint,
        router,
        max_peers,
        rendezvous_params: rdv,
        rung_rx,
        cohost,
        state_file,
        transport,
        multihop: multihop_handle,
        unicast_rx,
        // Set by the advertise path (cli::create / embed::create) before
        // `run`; absent for every non-advertising session.
        live_count: None,
        // Default to the CLI driver; the MCP / embed sessions
        // overwrite `cfg.driver` before handing it to `daemon::run`.
        driver: DriverMode::Cli,
        ready,
    })
}

/// The value cluster [`setup_create`] needs beyond the shared [`SetupBuild`]
/// handles — mirrors [`SetupKind::Create`]'s fields one-for-one.
struct CreateSetup {
    name: MeshName,
    config: MeshConfig,
    advertise: Option<MeshName>,
    password: Option<Password>,
    invite_only: bool,
}

/// Mint a new mesh: seed + optional password stretch, then stand up the
/// endpoint, gossip overlay and rendezvous. The origin co-hosts the
/// rendezvous immediately ([`CoHostPolicy::Eager`]): it has no bootstrap
/// dial to self-collide with, and an otherwise empty mesh needs a beacon
/// from t=0.
async fn setup_create(build: &SetupBuild<'_>, create: CreateSetup) -> Result<Assembled> {
    let CreateSetup {
        name,
        config,
        advertise,
        password,
        invite_only,
    } = create;
    let sink = build.sink;
    let author = build.author;

    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);

    let (endpoint, multihop) = build_member_endpoint(build).await?;

    let mut mesh = Mesh::new(seed, name.clone(), config);
    // Invite-only: mint the invite root + issuer keypair and bake the issuer
    // pubkey into the id BEFORE it is rendered. Cheap (random keys, no Argon2).
    if invite_only {
        mesh.set_invite();
    }
    // Password: on a plain mesh it stretches the topic (the verifier is baked
    // into the id) off the async worker; on an invite-only mesh the topic is
    // already gated by the invite root, so the password only wraps minted
    // invites — retained, never folded into derivation. Either way the raw
    // password is returned (not dropped) so the daemon can key tickets with it.
    let (mesh, mesh_password) = match (password, invite_only) {
        (Some(password), false) => {
            tokio::task::spawn_blocking(move || {
                let mut mesh = mesh;
                mesh.set_password(&password);
                (mesh, Some(password))
            })
            .await?
        }
        (password, _) => (mesh, password),
    };
    let mesh_key = mesh.stretched_key().map(zeroize::Zeroizing::new);
    // The creator alone retains its full mesh (issuer key + root) to mint from.
    let mint_mesh = mesh.requires_invite().then(|| mesh.clone());
    let id_str = mesh.to_string();
    let mesh_id =
        MeshId::new(id_str.clone()).expect("Mesh::to_string always produces a valid MeshId");

    sink.emit(NodeEvent::Info(format!(
        "created #{name} and joined as <{author}>"
    )));
    if mesh.requires_password() {
        sink.emit(NodeEvent::Info(
            "password-protected — joiners must present the password".to_owned(),
        ));
    }
    if mesh.requires_invite() {
        sink.emit(NodeEvent::Info(
            "invite-only — mint a 🎟️ invite with `agent-square invite` for joiners".to_owned(),
        ));
    }
    if let Some(directory) = &advertise {
        sink.emit(NodeEvent::Info(format!("advertising on #{directory}")));
    }
    sink.emit(NodeEvent::MeshId {
        id: mesh_id.clone(),
    });
    // `ready` is emitted by `run`, once the IPC socket accepts — not here.
    let topic_id = mesh.topic_id();
    lifecycle::log_ready(
        topic_id,
        name.as_str(),
        author.as_str(),
        mesh.network_label(),
    );

    let (gossip, router) = build_mesh(
        endpoint.clone(),
        build.max_peers,
        Some(build.unicast_acceptor.clone()),
    );
    // Creator has no peers yet — bootstrap is empty.
    let topic = gossip.subscribe(topic_id, vec![]).await?;

    let rdv = rendezvous_params(&mesh, topic_id, build.lookups, build.rung_tx.clone());
    register_rendezvous(&endpoint, &rdv);

    Ok(Assembled {
        mesh_id,
        mesh_name: name,
        endpoint,
        router,
        gossip,
        topic,
        rdv,
        cohost: CoHostPolicy::Eager,
        mesh_password,
        mesh_key,
        mint_mesh,
        multihop,
    })
}

/// Attach to an existing mesh. Join and Topic share one attach path;
/// they differ only in the co-host policy (Topic has no distinguished
/// creator, so its first peer must beacon) and the startup verb. Like
/// `create`, non-blocking on the *mesh*: the subscribe below only kicks off a
/// background connect, so a joiner is never invisible while bootstrapping and
/// `ready` never waits for a peer. It does wait for the IPC socket — see
/// `run`.
async fn setup_join(build: &SetupBuild<'_>, kind: SetupKind) -> Result<Assembled> {
    let sink = build.sink;
    let author = build.author;

    let (mesh, mesh_password, cohost, verb) = match kind {
        SetupKind::Join { mesh, password } => (mesh, password, CoHostPolicy::Deferred, "joined"),
        // A topic mesh is always passwordless.
        SetupKind::Topic { mesh } => (mesh, None, CoHostPolicy::EagerProbed, "joined topic"),
        SetupKind::Create { .. } => unreachable!("outer arm excludes Create"),
    };
    let mesh_key = mesh.stretched_key().map(zeroize::Zeroizing::new);
    let id_str = mesh.to_string();
    let mesh_id =
        MeshId::new(id_str.clone()).expect("Mesh::to_string always produces a valid MeshId");
    let topic_id = mesh.topic_id();

    let (endpoint, multihop) = build_member_endpoint(build).await?;

    let rdv = rendezvous_params(&mesh, topic_id, build.lookups, build.rung_tx.clone());
    // Must precede the join: the participant resolves the rendezvous id via
    // this registered address — the loopback port ladder (loopback-only) or
    // the chosen relay rung (reachable across machines).
    register_rendezvous(&endpoint, &rdv);

    let (gossip, router) = build_mesh(
        endpoint.clone(),
        build.max_peers,
        Some(build.unicast_acceptor.clone()),
    );
    // We subscribe, background-connect to the rendezvous, and — for a plain
    // join — `daemon::run` defers co-hosting our own (same seed-id) rendezvous
    // until we are meshed, so we never register a duplicate `rendezvous_id` on
    // the shared pinned relay that could capture our own bootstrap dial. A
    // topic instead claims eagerly (probe-first) so the first peer beacons.
    // See `EventLoopConfig::cohost`.
    let topic = gossip.subscribe(topic_id, vec![rdv.id]).await?;

    // `ready` is emitted by `run`, once the IPC socket accepts — not here.
    lifecycle::log_ready(
        topic_id,
        mesh.name.as_str(),
        author.as_str(),
        mesh.network_label(),
    );
    sink.emit(NodeEvent::Info(format!(
        "{verb} #{} as <{author}>",
        mesh.name
    )));

    Ok(Assembled {
        mesh_id,
        mesh_name: mesh.name,
        endpoint,
        router,
        gossip,
        topic,
        rdv,
        cohost,
        mesh_password,
        mesh_key,
        // A joiner holds no issuer key, so it can never mint — even after
        // redeeming an invite-only mesh.
        mint_mesh: None,
        multihop,
    })
}
