//! The `select!` event loop itself — the bulk of the daemon.
//!
//! [`run`] sets up per-session state then drives [`event_loop`], which
//! multiplexes external inputs (stdin / IPC / gossip), time-driven
//! maintenance (heartbeat, sweep, heal, anti-entropy, reclaim), and
//! shutdown. The orchestration lives here; the behavioral subsystems are
//! crate-root siblings (`crate::{gossip,lifecycle,beacon,lookup}`), and
//! the daemon-internal plumbing (`config`/`ctx`/`ipc`/`state`/`timers`/
//! `setup`) are siblings under `super`.

use std::time::{Duration, Instant};

use anyhow::Result;
use futures_util::StreamExt;
use iroh::{Endpoint, EndpointId, RelayUrl};
use iroh_gossip::api::{GossipReceiver, GossipSender};
use tokio::io::BufReader;
use tokio::sync::{broadcast, mpsc, watch};

use crate::daemon::state_file::StateFile;
use crate::gossip::event::{NodeEvent, NodeSink, PingRtt};
use crate::protocol::mesh::MeshName;
use crate::protocol::{MeshId, Message, Nickname};
use crate::transport::MeshSender;
use crate::transport::ipc::IpcMessage;
use crate::transport::spool;
use crate::util::bounded_read::{LineRead, read_bounded_line};
use crate::util::consts::MAX_STDIN_LINE_BYTES;
use crate::util::tuning::{
    ALIVE_INTERVAL_SECS, LINKSTATE_INTERVAL_SECS, RECLAIM_INTERVAL_MS, RESUBSCRIBE_MAX_ATTEMPTS,
    STATE_REFRESH_SECS, antientropy_interval_secs, heal_interval_secs, heal_stall_threshold_secs,
    ppid_watch_interval_ms, sweep_interval_secs,
};
use crate::{beacon, gossip, lifecycle, lookup};

use super::app::NodeDriver;
use super::config::{CoHostPolicy, DriverMode, EventLoopConfig};
use super::ctx::HandlerCtx;
use super::state::{EventLoopState, MeshSecrets};
use super::{setup, timers};
use crate::gossip::app::NodeApp;

/// Never returns normally — exits the process on ctrl-c / SIGTERM.
///
/// The engine loop is generic over the application: `app` implements
/// [`NodeDriver`] (the a2a layer's `A2aApp` is the only current impl), and its
/// two typed driver inputs — the in-process `session_rx` (embed/MCP) and the
/// localhost binding's `http_rx` (`--a2a-serve`) — are threaded here so the
/// loop never names an application payload type.
///
/// # Errors
/// Propagates a fatal setup/transport error that terminates the event loop.
#[expect(
    clippy::too_many_lines,
    reason = "session setup + secrets + sender/receiver wiring before handing off to event_loop; each step is a single call, and splitting further would only scatter sequential setup across helpers"
)]
pub async fn run<A: NodeDriver>(
    cfg: EventLoopConfig,
    app: A,
    session_rx: Option<mpsc::Receiver<A::Session>>,
    http_rx: Option<mpsc::Receiver<A::Http>>,
) -> Result<()> {
    let EventLoopConfig {
        topic,
        gossip,
        author,
        identity,
        mesh: mesh_str,
        name: mesh_name,
        mesh_password,
        mesh_key,
        sink,
        mint_mesh,
        interactive,
        endpoint,
        router: _router,
        max_peers,
        rendezvous_params,
        rung_rx,
        cohost,
        state_file,
        spool,
        transport,
        multihop,
        unicast_rx,
        live_count,
        driver,
    } = cfg;

    // Every driver-derived fact in one place. Only the CLI exits the
    // process on quit and binds the unix socket; in-process drivers
    // (embed / MCP) take typed requests on `session_rx` instead.
    let (external_quit_rx, external_msg_tx, ipc_listener_disabled, exit_on_quit, handle_signals) =
        match driver {
            DriverMode::Cli => (None, None, false, true, true),
            DriverMode::InProcess {
                msg_tx,
                quit_rx,
                handle_signals,
            } => (Some(quit_rx), msg_tx, true, false, handle_signals),
        };
    let external_req_rx = session_rx;
    let a2a_rx = http_rx;

    let started = Instant::now();
    // CLI `create`/`join` daemons default their state file into the mesh's
    // runtime folder (`<prefix>/<nick>.state.json`, beside the socket + log)
    // when no `--state-file` override is given. In-process embed/MCP sessions
    // (`!exit_on_quit`) keep writing nothing.
    let state_file = state_file
        .or_else(|| {
            exit_on_quit.then(|| {
                crate::util::mesh_runtime_dir(mesh_str.as_str())
                    .join(format!("{author}.state.json"))
            })
        })
        .map(|path| StateFile::new(path, &mesh_str, &author, &mesh_name));
    // Seed the state file with the app's discovery fields (the a2a bind port +
    // bearer token) before readiness is advertised — the local client reads them
    // from this mode-600 file.
    app.init_state_file(state_file.as_ref());
    let mut state = EventLoopState::new(
        state_file,
        started,
        identity,
        MeshSecrets {
            password: mesh_password,
            key: mesh_key,
        },
    );
    state.mint_mesh = mint_mesh; // creator-only: backs the `invite` command
    state.multihop = multihop; // `--multihop`: the registered transport's handle
    wire_session_state(
        &mut state,
        &endpoint,
        SessionWireParams {
            transport,
            live_count,
            rendezvous_id: rendezvous_params.id,
        },
    );

    // An eager member co-hosts from t=0 so a beacon exists before any
    // joiner subscribes; everyone else defers to the heal gate
    // (`may_cohost`). `Eager` skips the probe (a brand-new mesh has no
    // peers to self-collide with); `EagerProbed` probes first, so several
    // advertisers sharing one directory `rendezvous_id` don't bind
    // duplicate copies. Why: `EventLoopConfig::cohost`.
    let mut rendezvous: Option<beacon::Rendezvous> = None;
    if claims_at_startup(cohost) {
        beacon::ensure(
            &rendezvous_params,
            &endpoint,
            &mut rendezvous,
            probes_before_claim(cohost),
        )
        .await;
    }

    let (gossip_sender, receiver) = topic.split();

    // Wrap the sender, installing the `--spool` mirror when configured. A bad
    // spool dir aborts startup; the watcher guard must outlive the loop, so it
    // stays owned here (like `_router`). See `build_sender`.
    let (sender, spool_rx, _spool_watcher) = build_sender(spool, gossip_sender, &mesh_str)?;

    let ipc_rx = spawn_ipc_rx::<A::Ipc>(ipc_listener_disabled, &mesh_str, &author, &sink);

    // Arrival announce is deferred to the first `NeighborUp` — see
    // `gossip::handle_gossip_event`.

    let intervals = build_maintenance_intervals().await;
    let quit_rx = if handle_signals {
        spawn_quit_signal_tasks(exit_on_quit)
    } else {
        // A session inside a foreground command that owns its own lifetime
        // (a `--advertise` transfer, a directory browse) must not register
        // process-wide signal handlers — doing so suppresses the OS
        // default-terminate forever and the host command stops dying on
        // ctrl-c. Give the loop a quit channel that never fires instead;
        // shutdown comes from `external_quit_rx` / drop.
        let (quit_tx, quit_rx) = mpsc::channel::<()>(1);
        std::mem::forget(quit_tx);
        quit_rx
    };

    // Flip `ready` to `true` only once the daemon can actually serve, then
    // re-write the state file (the earlier write reported `ready: false`).
    // "Serving" means: in CLI mode the IPC socket is bound — `spawn_ipc_rx`
    // binds *synchronously*, so a `Some` receiver proves an accepting socket
    // exists and a gate that observes the flag is guaranteed a subsequent
    // `poll`/`msg` connect succeeds; a `None` here is a bind failure, and we
    // must NOT advertise readiness (the daemon still gossips, but has no IPC).
    // In-process mode (embed/MCP) has no socket by design (`req_rx` drives it)
    // and no `--state-file`, so it is always considered serving.
    if ipc_listener_disabled || ipc_rx.is_some() {
        state.ready = true;
        state.write_participant_count();
    }

    // `_router` stays owned in this scope so its accept loop outlives
    // the event loop below (dropping it makes the daemon unreachable
    // to new peers).
    //
    // `Box::pin` keeps the event-loop future off `run`'s stack frame, so
    // `run` — and every caller that awaits it up through `cli::dispatch`
    // — stays under clippy's `large_futures` threshold. The future's
    // size is target-dependent (it crosses the limit on x86_64-linux but
    // not aarch64-macOS), so boxing the single await is more robust than
    // shaving struct fields.
    Box::pin(event_loop(EventLoop {
        sender,
        receiver,
        gossip,
        endpoint,
        mesh: mesh_str,
        name: mesh_name,
        author,
        sink,
        max_peers,
        state,
        app,
        ipc_rx,
        interactive,
        intervals,
        rendezvous,
        rendezvous_params,
        rung_rx,
        cohost,
        started,
        external_quit_rx,
        external_req_rx,
        external_msg_tx,
        quit_rx,
        exit_on_quit,
        a2a_rx,
        unicast_rx: Some(unicast_rx),
        spool_rx,
    }))
    .await
}

/// The wrapped sender, plus the spool's inbound receiver and watcher guard when
/// `--spool` is active. Aliased so the return stays under `type_complexity`.
type SenderSetup = (
    MeshSender,
    Option<mpsc::Receiver<bytes::Bytes>>,
    Option<notify::RecommendedWatcher>,
);

/// Wrap the raw gossip sender, installing the `--spool` mirror when a directory
/// was configured. The returned watcher guard must be held for the loop's whole
/// lifetime; dropping it stops inbound spool ingestion.
fn build_sender(
    spool: Option<std::path::PathBuf>,
    gossip_sender: GossipSender,
    mesh_id: &MeshId,
) -> Result<SenderSetup> {
    let Some(dir) = spool else {
        return Ok((MeshSender::new(gossip_sender, None), None, None));
    };
    let (writer, inbound_rx, watcher) = spool::install(&dir, mesh_id.as_str())?.into_parts();
    Ok((
        MeshSender::new(gossip_sender, Some(writer)),
        Some(inbound_rx),
        Some(watcher),
    ))
}

/// The 1-minute housekeeping arm: the memory warn + reassembly sweep, then
/// ask authors to re-send what our stalled big shard groups are missing —
/// Wire the just-built state to this session's endpoint + config: the real
/// unicast pool (the default is detached), the transport policy, the
/// advertise counter (set before the first write so the initial ad carries a
/// real count), and the rendezvous id — then publish the initial count.
fn wire_session_state(state: &mut EventLoopState, endpoint: &Endpoint, wiring: SessionWireParams) {
    state.unicast_pool = crate::unicast::UnicastPool::new(endpoint.clone());
    state.transport = wiring.transport;
    state.live_count = wiring.live_count;
    state.rendezvous_id = Some(wiring.rendezvous_id);
    state.write_participant_count();
}

/// The per-session value cluster [`wire_session_state`] folds into a fresh
/// [`EventLoopState`]: the configured transport policy, the shared advertise
/// counter (if advertising), and the well-known rendezvous endpoint id.
struct SessionWireParams {
    transport: crate::transport::TransportPolicy,
    live_count: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    rendezvous_id: EndpointId,
}

/// The alive tick: note the gap, then broadcast the keepalive presence.
async fn alive_arm(anchors: &mut TickAnchors, state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
    timers::note_tick_gap(
        "alive",
        &mut anchors.alive,
        &mut anchors.alive_wall,
        Duration::from_secs(ALIVE_INTERVAL_SECS),
    );
    lifecycle::heartbeat::tick_alive(state, ctx.sender, ctx.mesh, ctx.author).await;
}

/// The anti-entropy tick: note the gap, advertise the chat digest, then both
/// channel digests, so peers can request anything we hold that they miss.
async fn antientropy_arm(
    anchors: &mut TickAnchors,
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
) {
    timers::note_tick_gap(
        "antientropy",
        &mut anchors.antientropy,
        &mut anchors.antientropy_wall,
        Duration::from_secs(antientropy_interval_secs()),
    );
    gossip::antientropy::broadcast_digest(state, ctx.sender, ctx.mesh, ctx.author).await;
    gossip::antientropy::broadcast_state_digests(state, ctx.sender, ctx.mesh, ctx.author).await;
}

/// Default per-link routing cost we advertise for our own neighbours until live
/// telemetry (RTT / delivery) is wired into the multihop metric.
const MULTIHOP_LINK_COST: u32 = 10;

/// The multihop link-state tick: re-broadcast our own links (one per direct
/// neighbour, carrying our underlay dial address) so every peer keeps a fresh
/// routing graph for the multihop transport. No-op until meshed, or when the
/// multihop transport is off — a vector with no consumer helps no one.
async fn linkstate_arm(state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
    if !state.meshed || state.multihop.is_none() {
        return;
    }
    state.link_state_seq += 1;
    let seq = state.link_state_seq;
    let links: Vec<_> = state
        .linked_endpoints
        .iter()
        .map(|eid| (*eid, MULTIHOP_LINK_COST))
        .collect();
    let handle = state.multihop.as_ref().expect("checked above");
    let vector = handle.link_vector(seq, links);
    // Fold our own vector into our own routing table: gossip never loops a
    // broadcast back, and without our outbound edges the local graph can't
    // source a route (`route_to(self, …)` would always be empty).
    handle.feed_topology(vector.clone());
    let Ok(json) = serde_json::to_string(&vector) else {
        return;
    };
    let Ok(body) = crate::protocol::MessageBody::new(json) else {
        return;
    };
    gossip::broadcast_msg(
        ctx.sender,
        &Message::new_link_state(ctx.mesh, ctx.author, body).signed(&state.identity),
    )
    .await;
}

/// The sweep-tick arm: note the gap, then evict silent peers. The app's own
/// timers that ride this cadence run via [`NodeDriver::on_tick`] right after.
fn sweep_arm(anchors: &mut TickAnchors, state: &mut EventLoopState, sink: &dyn NodeSink) {
    timers::note_tick_gap(
        "sweep",
        &mut anchors.sweep,
        &mut anchors.sweep_wall,
        Duration::from_secs(sweep_interval_secs()),
    );
    lifecycle::heartbeat::tick_sweep(state, sink);
}

/// Owned working set for [`event_loop`]. `run` does setup, fills this,
/// and hands it over; the loop destructures it back into the same
/// locals the orchestrator used to hold inline. Splitting the 11-arm
/// `select!` out keeps both functions within the readability budget
/// (clippy `too_many_lines`) without an `#[allow]`.
struct EventLoop<A: NodeDriver> {
    sender: MeshSender,
    receiver: GossipReceiver,
    /// The gossip frontend, kept so the loop can re-subscribe the topic
    /// after the stream terminally ends (see the heal arm) — without it
    /// a closed subscription (e.g. lag-evicted by the actor) would
    /// leave the daemon permanently deaf.
    gossip: iroh_gossip::net::Gossip,
    endpoint: Endpoint,
    mesh: MeshId,
    name: MeshName,
    author: Nickname,
    sink: std::sync::Arc<dyn NodeSink>,
    max_peers: usize,
    state: EventLoopState,
    /// Application state behind the [`NodeDriver`] seam, disjoint from the mesh
    /// `state` and threaded as its own `&mut` through the app callbacks.
    app: A,
    ipc_rx: Option<mpsc::Receiver<IpcMessage<A::Ipc>>>,
    interactive: bool,
    intervals: MaintenanceIntervals,
    rendezvous: Option<beacon::Rendezvous>,
    rendezvous_params: beacon::RendezvousParams,
    /// Bootstrap rung chosen off-loop (startup probe + beacon
    /// self-monitor); the loop applies changes via the rung-update arm.
    rung_rx: watch::Receiver<Option<RelayUrl>>,
    /// When this member may serve the rendezvous (see [`CoHostPolicy`]).
    cohost: CoHostPolicy,
    /// Event-loop start, for the unmeshed-joiner co-host grace.
    started: Instant,
    external_quit_rx: Option<mpsc::Receiver<()>>,
    /// The in-process typed session channel (embed / MCP); `None` on the CLI.
    external_req_rx: Option<mpsc::Receiver<A::Session>>,
    external_msg_tx: Option<broadcast::Sender<Message>>,
    quit_rx: mpsc::Receiver<()>,
    exit_on_quit: bool,
    /// The localhost A2A binding's request channel (`--a2a-serve`); `None` off.
    a2a_rx: Option<mpsc::Receiver<A::Http>>,
    /// Inbound unicast frames from the `UNICAST_ALPN` acceptor, drained into
    /// `gossip::ingest` (same validation + dedup path as gossip). `Option` so
    /// the `select!` arm can disable itself if the channel ever closes.
    unicast_rx: Option<mpsc::Receiver<bytes::Bytes>>,
    /// Inbound frames the `--spool` watcher read from the shared directory,
    /// drained into `gossip::ingest` (same path as gossip/unicast). `None` when
    /// no spool is active; the arm disables itself if the channel closes.
    spool_rx: Option<mpsc::Receiver<bytes::Bytes>>,
}

/// The daemon's `select!` loop. Never returns normally on the CLI
/// path (ctrl-c / SIGTERM `std::process::exit`s); embedded drivers
/// break out via their external quit channel and get `Ok(())`.
/// Log the one per-daemon build-stamp line into the always-on file (one log
/// file == one process == one build). The `ready` JSON event carries the same
/// `version`; this is the file-log counterpart. Explicit pinned target so it
/// survives a release build's `error` base.
fn log_daemon_start(author: &Nickname) {
    tracing::info!(
        target: "agent_square::lifecycle",
        version = crate::VERSION,
        nickname = %author,
        "daemon starting"
    );
}

#[expect(
    clippy::too_many_lines,
    reason = "the daemon's central select! loop: one arm per event source (stdin, ipc, a2a, gossip, circuit, the maintenance ticks, quit); each arm delegates to a helper, but the arm list itself is irreducibly long"
)]
async fn event_loop<A: NodeDriver>(loop_state: EventLoop<A>) -> Result<()> {
    let EventLoop {
        mut sender,
        mut receiver,
        gossip,
        endpoint,
        mesh: mesh_str,
        name: mesh_name,
        author,
        sink,
        max_peers,
        mut state,
        mut app,
        mut ipc_rx,
        interactive,
        mut intervals,
        mut rendezvous,
        mut rendezvous_params,
        mut rung_rx,
        cohost,
        started,
        mut external_quit_rx,
        mut external_req_rx,
        external_msg_tx,
        mut quit_rx,
        exit_on_quit,
        mut a2a_rx,
        mut unicast_rx,
        mut spool_rx,
    } = loop_state;

    log_daemon_start(&author);

    // The surfaced-events ring (`poll`/`fetch` history) lives app-side, fed by
    // the tap the caller attached to `output` before handing it in (both this
    // `output` and the app's own `Output` are clones of that one tapped sink,
    // so events interleave in surfacing order). The loop drives the ring
    // through the app's `drain_surfaced` / poll-deadline hooks — the engine
    // never names the ring's element type.

    let mut stdin_reader = BufReader::new(tokio::io::stdin());
    let mut stdin_open = interactive;

    let mut anchors = TickAnchors::now();

    // Owned Arc clone so the per-arm ctx can borrow it without colliding with `&mut state`.
    let identity = state.identity.clone();
    // Our own pubkey hex, computed once for the per-message self-echo compare.
    let our_pubkey = crate::protocol::identity::encode_pubkey(&identity.public());
    // Everything a HandlerCtx needs *except* the sender. The ctx itself
    // is built per-arm (`parts.ctx(&sender)`) rather than once out here:
    // a loop-lifetime ctx would borrow `sender` forever, and the
    // resubscribe path must replace `sender`/`receiver` when the gossip
    // stream ends.
    let parts = CtxParts {
        endpoint: &endpoint,
        mesh: &mesh_str,
        author: &author,
        identity: identity.as_ref(),
        our_pubkey: &our_pubkey,
        max_peers,
        rendezvous_id: rendezvous_params.id,
        external_msg_tx: external_msg_tx.as_ref(),
        sink: sink.as_ref(),
    };

    // Consecutive failed resubscribe attempts (reset on success); at
    // `RESUBSCRIBE_MAX_ATTEMPTS` the gossip actor itself is gone and the
    // daemon shuts down rather than pretend to be a live member.
    let mut resubscribe_attempts: u32 = 0;

    {
        let ctx = parts.ctx(&sender);
        app.on_startup(&mut state, &ctx).await;
    }

    loop {
        tokio::select! {
            result = read_bounded_line(&mut stdin_reader, MAX_STDIN_LINE_BYTES), if stdin_open =>
                stdin_open = handle_stdin_arm(result, &mut app, &mut state, &parts.ctx(&sender)).await,
            () = sleep_until_opt(state.ping_round.as_ref().map(|round| round.deadline)) =>
                finalize_ping_round(&mut state, sink.as_ref()),
            () = sleep_until_opt(app.earliest_poll_deadline()) => app.poll_deadline_elapsed(),
            () = sleep_until_opt(app.earliest_deadline()) =>
                app.expire_deadlines(tokio::time::Instant::now()),
            ipc_msg = recv_opt(&mut ipc_rx) => match ipc_msg {
                None => ipc_rx = None,
                Some((cmd, resp_tx)) => {
                    let ctx = parts.ctx(&sender);
                    let req = super::app::IpcRequest {
                        cmd,
                        resp: resp_tx,
                        name: &mesh_name,
                    };
                    if app.handle_ipc(req, &mut state, &ctx).await {
                        state.last_sent_at = Instant::now();
                    }
                }
            },
            a2a_req = recv_opt(&mut a2a_rx) => match a2a_req {
                None => a2a_rx = None,
                Some(req) => {
                    let ctx = parts.ctx(&sender);
                    app.handle_http(req, &mut state, &ctx).await;
                }
            },
            event = receiver.next(), if state.gossip_open => {
                let ctx = parts.ctx(&sender);
                gossip::handle_gossip_event(event, &mut state, &mut app, &ctx).await;
            }
            // Inbound unicast rides the *same* validate + dedup path as gossip (`ingest`).
            frame = recv_opt(&mut unicast_rx) => match frame {
                Some(bytes) => gossip::ingest(bytes, &mut state, &mut app, &parts.ctx(&sender)).await,
                None => unicast_rx = None,
            },
            // Inbound spool frames (files peers wrote to the shared dir) ride
            // the same `ingest` path — self-echo + verify + dedup drop our own
            // mirrored frames and any duplicates.
            frame = recv_opt(&mut spool_rx) => match frame {
                Some(bytes) => gossip::ingest(bytes, &mut state, &mut app, &parts.ctx(&sender)).await,
                None => spool_rx = None,
            },
            _ = intervals.prune.tick() => timers::tick_prune(&mut state, sink.as_ref()),
            _ = intervals.alive.tick() => {
                let ctx = parts.ctx(&sender);
                alive_arm(&mut anchors, &mut state, &ctx).await;
            }
            _ = intervals.sweep.tick() => {
                sweep_arm(&mut anchors, &mut state, sink.as_ref());
                let ctx = parts.ctx(&sender);
                app.on_tick(&mut state, &ctx).await;
            }
            _ = intervals.heal.tick() => {
                let (mono_gap, wall_gap) = timers::note_tick_gap("heal", &mut anchors.heal, &mut anchors.heal_wall, Duration::from_secs(heal_interval_secs()));
                if state.gossip_open {
                    let ctx = parts.ctx(&sender);
                    heal_tick(&mut state, &ctx, HealTickParams {
                        gap: TickGap { mono: mono_gap, wall: wall_gap },
                        params: &rendezvous_params,
                        cohost,
                        started,
                    }, &mut rendezvous).await;
                } else {
                    // Stream ended: resubscribe instead of healing a dead topic
                    // (see `resubscribe_tick`); the beacon keeps the mesh joinable.
                    resubscribe_tick(
                        &ResubscribeEnv { gossip: &gossip, params: &rendezvous_params, parts: &parts, exit_on_quit },
                        &mut state,
                        &mut app,
                        GossipLink { sender: &mut sender, receiver: &mut receiver, attempts: &mut resubscribe_attempts },
                    ).await?;
                    let ctx = parts.ctx(&sender);
                    maybe_cohost(&state, &ctx, &CohostArm { policy: cohost, params: &rendezvous_params, started }, &mut rendezvous).await;
                }
            }
            // A bootstrap rung chosen off-loop (startup probe / beacon self-monitor); apply it cheaply.
            // `Ok(())` only: a closed channel (impossible while the beacon params live) disables the arm.
            Ok(()) = rung_rx.changed() => apply_rung_change(&mut rendezvous_params, &endpoint, &mut rendezvous, &rung_rx),
            _ = intervals.reclaim.tick() => {
                let ctx = parts.ctx(&sender);
                maybe_reclaim(&state, &ctx, &CohostArm { policy: cohost, params: &rendezvous_params, started }, &mut rendezvous).await;
            }
            _ = intervals.antientropy.tick() => {
                let ctx = parts.ctx(&sender);
                antientropy_arm(&mut anchors, &mut state, &ctx).await;
            }
            _ = intervals.state_refresh.tick() => timers::tick_state_refresh(&state, &endpoint).await,
            _ = intervals.linkstate.tick() => {
                let ctx = parts.ctx(&sender);
                linkstate_arm(&mut state, &ctx).await;
            }
            _ = recv_opt(&mut external_quit_rx) => {
                // External quit is always embedded (MCP): never hard-exit (`false`).
                let ctx = parts.ctx(&sender);
                announce_and_maybe_exit(&mut state, &mut app, &ctx, QuitParams { name: &mesh_name, exit_on_quit: false }).await;
                break;
            }
            req = recv_opt(&mut external_req_rx) => match req {
                None => external_req_rx = None,
                Some(req) => {
                    let ctx = parts.ctx(&sender);
                    if app.handle_session(req, &mut state, &ctx).await {
                        state.last_sent_at = Instant::now();
                    }
                }
            },
            _ = quit_rx.recv() => {
                let ctx = parts.ctx(&sender);
                announce_and_maybe_exit(&mut state, &mut app, &ctx, QuitParams { name: &mesh_name, exit_on_quit }).await;
                break;
            }
        }
        app.drain_surfaced();
    }

    Ok(())
}

/// Graceful shutdown: remove the statusline state file first, then
/// announce `Left` and give the broadcast a moment to reach peers.
/// Shared by both the external-quit and ctrl-c/SIGTERM/SIGHUP paths so
/// they can't drift apart.
///
/// The state-file removal is the time-critical step: an external reader
/// (the shell statusline) shows the mesh pill while the file is fresh,
/// so a leaver must clear it *immediately*. It runs before the
/// best-effort `Left` broadcast and its 500 ms propagation sleep so a
/// kill landing during that window can't strand the file with a still-fresh
/// `last_updated`, leaving a ghost pill on the statusline.
async fn shutdown<A: NodeDriver>(
    state: &mut EventLoopState,
    app: &mut A,
    ctx: &HandlerCtx<'_>,
    name: &MeshName,
) {
    // Release app-owned resources (fail parked app waiters, close the
    // blob-serving endpoint whose store spool is dropped with it).
    app.on_shutdown(state).await;
    if let Some(sf) = state.state_file.as_ref() {
        sf.remove();
    }
    ctx.sink.emit(NodeEvent::Info(format!("left #{name}")));
    lifecycle::log_leaving(name.as_str());
    gossip::broadcast_msg(
        ctx.sender,
        &Message::new_left(ctx.mesh, ctx.author).signed(&state.identity),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
}

/// The mesh name (for the departure log line) and whether this quit should
/// hard-exit the process — the plain values [`announce_and_maybe_exit`] needs
/// beyond the loop state and the shared handler context.
struct QuitParams<'a> {
    name: &'a MeshName,
    exit_on_quit: bool,
}

/// Announce departure, then decide whether to hard-exit the process.
///
/// `exit_on_quit` is the CLI hard-exit: in interactive CLI mode the blocking
/// stdin reader thread won't terminate on its own, so we `process::exit`.
/// Embedded/MCP quits pass `false` and unwind cleanly instead. Under the
/// `dhat-heap` profiling build we *never* `process::exit` regardless — it skips
/// destructors, so the heap profiler would never flush `dhat-heap.json`; we fall
/// through so `main` unwinds and the profiler drops (safe because profiling runs
/// use `--no-interactive`, i.e. no blocking stdin thread to hang shutdown).
async fn announce_and_maybe_exit<A: NodeDriver>(
    state: &mut EventLoopState,
    app: &mut A,
    ctx: &HandlerCtx<'_>,
    quit: QuitParams<'_>,
) {
    // Empty out any parked long-poll waiters first, so a held call returns a
    // clean timeout (empty) rather than a dropped-channel error — and before
    // the `exit_on_quit` path below may `std::process::exit`. Other app-owned
    // waiters (A2A calls) are failed in `on_shutdown` (inside `shutdown`).
    app.close_poll_waiters();
    shutdown(state, app, ctx, quit.name).await;
    // Drain any frames still queued for the spool before we abort the process —
    // otherwise `process::exit` kills the detached writer mid-flush and a
    // burst-then-quit loses its tail (a sneakernet hand-off's last messages).
    // Bounded so a wedged disk can't hang shutdown.
    let _ = tokio::time::timeout(
        Duration::from_secs(crate::util::consts::SPOOL_FLUSH_TIMEOUT_SECS),
        ctx.sender.flush(),
    )
    .await;
    #[cfg(not(feature = "dhat-heap"))]
    if quit.exit_on_quit {
        std::process::exit(0);
    }
    #[cfg(feature = "dhat-heap")]
    let _ = quit.exit_on_quit;
}

/// Spawn ctrl-c (all platforms) plus SIGTERM/SIGHUP/SIGQUIT (unix)
/// listener tasks feeding a single internal quit channel.
/// `tokio::signal::ctrl_c()` inside a `select!` branch doesn't reliably
/// interrupt a blocking stdin read, so we offload signal listening to
/// dedicated tasks.
///
/// Every catchable termination signal routes through the graceful
/// `shutdown()` path so the statusline state file is removed. SIGHUP in
/// particular is what a closing parent (e.g. the Monitor that hosts the
/// daemon for a `/square:*` session) tends to send; without catching it
/// the default action terminated the daemon without cleanup, stranding a
/// ghost pill on the statusline. Only SIGKILL stays uncatchable.
fn spawn_quit_signal_tasks(exit_on_quit: bool) -> mpsc::Receiver<()> {
    let (quit_tx, quit_rx) = mpsc::channel::<()>(1);
    let ctrl_c_tx = quit_tx.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = ctrl_c_tx.send(()).await;
    });
    #[cfg(unix)]
    for kind in [
        tokio::signal::unix::SignalKind::terminate(),
        tokio::signal::unix::SignalKind::hangup(),
        tokio::signal::unix::SignalKind::quit(),
    ] {
        let signal_tx = quit_tx.clone();
        tokio::spawn(async move {
            let mut signal =
                tokio::signal::unix::signal(kind).expect("failed to register termination handler");
            signal.recv().await;
            let _ = signal_tx.send(()).await;
        });
    }
    // Only the CLI daemon owns a process to exit; the embed/MCP driver runs
    // in-process with no parent of its own to lose, so it must never self-quit
    // on a host reparent.
    #[cfg(unix)]
    if exit_on_quit {
        spawn_orphan_watch(quit_tx);
    }
    quit_rx
}

/// Detect orphaning by the spawning agent and route it through the same quit
/// channel as a signal. A hard-killed parent (`kill -9`, a reinstall, an IDE
/// restart) can't run any cleanup, so the spawned daemon is reparented instead
/// of terminated and would otherwise linger in the mesh forever. The daemon
/// watches its *own* parent — the only mechanism that survives SIGKILL and is
/// identical on macOS and Linux (`PR_SET_PDEATHSIG` and kqueue `NOTE_EXIT` are
/// each platform-specific). When the parent vanishes we feed `quit_tx`, reusing
/// the SIGTERM path that broadcasts `left` and exits cleanly.
#[cfg(unix)]
#[expect(
    unsafe_code,
    reason = "libc::getppid FFI; no safe wrapper, always succeeds"
)]
fn spawn_orphan_watch(quit_tx: mpsc::Sender<()>) {
    let original_ppid = unsafe { libc::getppid() };
    if !orphan_watch_warranted(original_ppid) {
        return;
    }
    let interval = Duration::from_millis(ppid_watch_interval_ms());
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            let current_ppid = unsafe { libc::getppid() };
            if parent_lost(original_ppid, current_ppid) {
                let _ = quit_tx.send(()).await;
                return;
            }
        }
    });
}

/// Whether the orphan watch is worth running. Skip it when the daemon already
/// has no agent to lose — a parent pid of 1 means it was launched detached
/// straight from init/launchd, so it must never self-terminate.
#[cfg(unix)]
fn orphan_watch_warranted(original_ppid: i32) -> bool {
    original_ppid > 1
}

/// The orphaning test: the parent pid changed from the one captured at startup.
/// Comparing against the *original* (not against `1`) is what makes this correct
/// on both platforms — macOS reparents an orphan to launchd (1), but under
/// systemd Linux reparents to a subreaper at some other pid. Pid reuse can't
/// fool it: the reaper's pid won't coincidentally equal the original parent's.
#[cfg(unix)]
fn parent_lost(original_ppid: i32, current_ppid: i32) -> bool {
    original_ppid != current_ppid
}

/// Resolve the IPC receiver: reuse a pre-wired channel (MCP/embed) or,
/// for the CLI, spawn the unix-socket listener and own the channel.
/// Returning `Option` keeps the loop's `select!` arm uniform.
fn spawn_ipc_rx<C: serde::de::DeserializeOwned + Send + 'static>(
    disable_ipc_listener: bool,
    mesh: &MeshId,
    author: &Nickname,
    sink: &std::sync::Arc<dyn NodeSink>,
) -> Option<mpsc::Receiver<IpcMessage<C>>> {
    // In-process mode (embed / MCP): no socket. Returning `None` leaves
    // the loop's IPC `select!` arm inert (it pends forever), so the
    // unix-socket listener is never bound — those drivers use the typed
    // `session_rx` instead.
    if disable_ipc_listener {
        return None;
    }
    // Bind synchronously here, *before* the caller marks the session ready,
    // so an accepting socket always exists by the time the readiness flag
    // flips. Only the (always-running) accept loop is spawned. A bind
    // failure is non-fatal — the daemon still gossips; it just can't take
    // IPC — matching the prior best-effort behavior, only now observed
    // before readiness rather than racing it.
    let listener = match crate::transport::ipc::bind(mesh, author) {
        Ok(listener) => listener,
        Err(error) => {
            sink.emit(NodeEvent::Error(format!("IPC: {error}")));
            tracing::warn!(%error, "IPC: failed to bind socket");
            return None;
        }
    };
    let (ipc_tx, rx) = mpsc::channel::<IpcMessage<C>>(32);
    tokio::spawn(crate::transport::ipc::serve::<C>(
        listener,
        ipc_tx,
        std::sync::Arc::clone(sink),
    ));
    Some(rx)
}

/// Sleep until a ping round's deadline, or pend forever when no round
/// is active. Lets the event loop's `select!` carry a ping-finalize arm
/// that only fires while a round is in flight, without borrowing
/// `state` across the await (the deadline is copied out beforehand).
async fn sleep_until_opt(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending::<()>().await,
    }
}

/// Build and emit the `ping_report` for the elapsed round, then clear
/// it. RTT is each pong's local arrival minus the probe broadcast time.
fn finalize_ping_round(state: &mut EventLoopState, sink: &dyn NodeSink) {
    let Some(round) = state.ping_round.take() else {
        return;
    };
    // (Live per-neighbour RTT/delivery telemetry once fed the circuit metric; the
    // multihop transport currently advertises a flat link cost, so the ping round
    // only produces the user-facing report below. Re-wiring telemetry into the
    // multihop metric is a future enhancement.)
    let mut peers: Vec<PingRtt> = round
        .pongs
        .iter()
        .map(|(nickname, arrival)| PingRtt {
            nickname: nickname.clone(),
            rtt_ms: u64::try_from(arrival.duration_since(round.t1).as_millis()).unwrap_or(u64::MAX),
        })
        .collect();
    peers.sort_by(|left, right| left.nickname.as_str().cmp(right.nickname.as_str()));
    // The embed/MCP `ping` request waits on this channel (no event stream to
    // read the report from); the CLI/IPC path leaves it unset and consumes the
    // `ping_report` event below instead.
    if let Some(resp) = round.resp {
        let _ = resp.send(peers.clone());
    }
    // `known` must never be less than the number that responded: a peer can
    // pong and then leave the roster before this ~10s finalize, which would
    // otherwise report responded > known. Clamp so the count stays coherent.
    let known = state.participants.len().max(peers.len());
    sink.emit(NodeEvent::PingReport { peers, known });
}

/// Whether a co-hosting member probes the rendezvous before claiming it —
/// the single source of truth for the `probe_first` flag passed to
/// [`beacon::ensure`] from every claim site (startup, heal tick, reclaim
/// window). Only `Eager` (the mesh origin) skips the probe: a brand-new
/// mesh has no peers to self-collide with. Every other policy probes, so
/// it never binds a duplicate of a rendezvous a peer already serves — the
/// directory advertiser's shared `rendezvous_id` (`EagerProbed`) or a
/// survivor mid-failover (`Deferred`). Exhaustive on purpose: a new variant
/// must make this decision explicitly rather than defaulting to "probe".
fn probes_before_claim(cohost: CoHostPolicy) -> bool {
    match cohost {
        CoHostPolicy::Eager => false,
        CoHostPolicy::EagerProbed | CoHostPolicy::Deferred | CoHostPolicy::Never => true,
    }
}

/// Whether this member claims the rendezvous **at startup** (t=0) rather
/// than deferring to the heal gate ([`may_cohost`]) or never co-hosting.
/// The eager policies claim immediately so a beacon exists before any
/// joiner/discoverer subscribes; whether that claim probes first is the
/// orthogonal [`probes_before_claim`] axis.
fn claims_at_startup(cohost: CoHostPolicy) -> bool {
    match cohost {
        CoHostPolicy::Eager | CoHostPolicy::EagerProbed => true,
        CoHostPolicy::Deferred | CoHostPolicy::Never => false,
    }
}

/// May this member co-host the rendezvous yet? See [`CoHostPolicy`].
/// `Never` never co-hosts (a pure consumer); `Eager`/`EagerProbed` always
/// may; a `Deferred` member only once `meshed`, or after
/// `cohost_grace_secs` for an empty mesh (then probe-gated in
/// `beacon::ensure`). Pure + cheap; never blocks `ready`.
fn may_cohost(cohost: CoHostPolicy, meshed: bool, started: Instant) -> bool {
    match cohost {
        CoHostPolicy::Never => false,
        CoHostPolicy::Eager | CoHostPolicy::EagerProbed => true,
        CoHostPolicy::Deferred => {
            meshed || started.elapsed().as_secs() >= crate::util::tuning::cohost_grace_secs()
        }
    }
}

/// Monotonic `gap` past `stall_threshold`: the process was throttled
/// (but not fully frozen) between heal ticks (macOS App Nap / timer
/// coalescing) long enough that the mesh died of idle timeout.
fn is_resume(gap: Duration, stall_threshold: Duration) -> bool {
    gap > stall_threshold
}

/// The macOS-sleep signature the monotonic gap is blind to: the
/// monotonic clock pauses in lockstep with the frozen process, so a
/// day-long suspend shows only a few seconds of `mono_gap` while the
/// wall clock jumped the whole way. A `wall_gap` exceeding `mono_gap`
/// by more than `stall_threshold` means time elapsed that the process
/// could not observe — it was suspended and the mesh is dead.
fn is_wall_resume(wall_gap: Duration, mono_gap: Duration, stall_threshold: Duration) -> bool {
    wall_gap.saturating_sub(mono_gap) > stall_threshold
}

/// One heal tick (factored out of `event_loop` for the line budget).
/// On a resume edge the steady probe can't rebuild a mesh that fully
/// died while the timers were frozen, so re-enter cold-joiner mode,
/// re-assert the relay-homed rendezvous hint (the network changed),
/// and run the long re-bootstrap probe. Otherwise the normal probe.
///
/// A resume is either a monotonic stall (throttle) OR a wall-vs-
/// monotonic divergence (suspend/sleep) — the latter is the only
/// signal that survives a macOS sleep, which freezes the monotonic
/// clock with the process.
async fn run_heal(
    gap: TickGap,
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
    params: &beacon::RendezvousParams,
) {
    let threshold = Duration::from_secs(heal_stall_threshold_secs());
    let hard_edge = is_resume(gap.mono, threshold) || is_wall_resume(gap.wall, gap.mono, threshold);
    if hard_edge {
        tracing::warn!(
            target: "agent_square::gossip",
            mono_gap_ms = u64::try_from(gap.mono.as_millis()).unwrap_or(u64::MAX),
            wall_gap_ms = u64::try_from(gap.wall.as_millis()).unwrap_or(u64::MAX),
            "heal: hard re-bootstrap edge"
        );
        state.note_degraded();
        // The frozen-era link view is stale by definition; clearing this
        // re-arms the regular tick's probe until a fresh NeighborUp.
        state.rendezvous_linked = false;
        // Re-assert the rendezvous hint (the network changed). The rung
        // is re-validated off-loop by the beacon's liveness self-monitor,
        // so a rung that died during the freeze self-corrects — no inline
        // ladder walk on the event loop here.
        setup::register_rendezvous(ctx.endpoint, params);
        gossip::heal::tick_heal_hard(ctx.endpoint, params.id, ctx.sender).await;
    } else if state.rendezvous_linked {
        // A live rendezvous link has nothing to heal — and healing it
        // anyway is what flapped it once per tick (both heal legs dial
        // `GOSSIP_ALPN`, which the beacon's gossip adopts, superseding
        // the healthy link; see `tick_heal`). `NeighborDown` re-arms
        // this gate instantly.
        tracing::debug!(
            target: "agent_square::gossip",
            "heal tick: rendezvous linked; idle"
        );
    } else {
        gossip::heal::tick_heal(params.id, ctx.sender).await;
    }
    // Rendezvous-independent re-bridge. Fires on the hard (resume) edge —
    // where a reused endpoint id can be stuck behind a stale *accepted*
    // rendezvous connection (iroh-gossip#10), so the rendezvous re-graft
    // alone may not re-admit us — or on steady-state loss of every live
    // link (relay flap). Re-dials remembered peers directly. Skipped when
    // healthy (`hard_edge` false and links remain) and for a lone node
    // (nothing remembered), so it adds no churn. `linked_endpoints` is
    // not cleared on the resume edge, hence the explicit `hard_edge` arm.
    if (hard_edge || state.linked_endpoints.is_empty()) && !state.known_endpoints.is_empty() {
        gossip::heal::rebridge_known(ctx.sender, &state.known_endpoints).await;
    }
    // Starvation watchdog: links/heal can look busy while no traffic
    // flows (the roster-collapse signature), so the last word every heal
    // tick is a check on verified *inbound* silence.
    if state.starvation_due(
        Instant::now(),
        Duration::from_secs(crate::util::tuning::starvation_threshold_secs()),
    ) {
        gossip::heal::recover_from_starvation(state, ctx).await;
    }
}

/// A tick's elapsed-time gap on both clocks: monotonic (throttle-blind) and
/// wall (the only one a macOS suspend shows up on) — see [`is_resume`] /
/// [`is_wall_resume`].
#[derive(Clone, Copy)]
struct TickGap {
    mono: Duration,
    wall: Duration,
}

/// The value cluster [`heal_tick`] needs beyond the loop state and handler
/// context: the tick's gap, the rendezvous params to heal/claim under, which
/// co-host policy governs this session, and (for the unmeshed-joiner grace)
/// when the event loop started.
struct HealTickParams<'a> {
    gap: TickGap,
    params: &'a beacon::RendezvousParams,
    cohost: CoHostPolicy,
    started: Instant,
}

/// One heal tick: re-bootstrap/heal, then (re)claim the beacon if we
/// should co-host. Grouped so the event-loop arm stays a one-liner.
async fn heal_tick(
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
    tick: HealTickParams<'_>,
    rendezvous: &mut Option<beacon::Rendezvous>,
) {
    run_heal(tick.gap, state, ctx, tick.params).await;
    let arm = CohostArm {
        policy: tick.cohost,
        params: tick.params,
        started: tick.started,
    };
    maybe_cohost(state, ctx, &arm, rendezvous).await;
}

/// Per-timer gap anchors; the heal gap also drives the resume-edge hard
/// re-bootstrap. Each timer carries a monotonic anchor AND a wall-clock
/// anchor: on macOS the monotonic clock pauses in lockstep with a
/// sleeping process, so only the wall gap reveals a suspend (see
/// `note_tick_gap` / `run_heal`).
struct TickAnchors {
    alive: Instant,
    sweep: Instant,
    heal: Instant,
    antientropy: Instant,
    alive_wall: i64,
    sweep_wall: i64,
    heal_wall: i64,
    antientropy_wall: i64,
}

impl TickAnchors {
    fn now() -> Self {
        let mono = Instant::now();
        let wall = crate::util::clock::unix_secs();
        Self {
            alive: mono,
            sweep: mono,
            heal: mono,
            antientropy: mono,
            alive_wall: wall,
            sweep_wall: wall,
            heal_wall: wall,
            antientropy_wall: wall,
        }
    }
}

/// Everything a [`HandlerCtx`] needs except the gossip sender. The loop
/// holds one of these and builds the ctx per-arm (`parts.ctx(&sender)`):
/// a loop-lifetime ctx would borrow `sender` forever, and the
/// resubscribe path must be able to replace it.
struct CtxParts<'a> {
    endpoint: &'a Endpoint,
    mesh: &'a MeshId,
    author: &'a Nickname,
    identity: &'a crate::protocol::identity::Identity,
    our_pubkey: &'a str,
    max_peers: usize,
    rendezvous_id: EndpointId,
    external_msg_tx: Option<&'a broadcast::Sender<Message>>,
    sink: &'a dyn NodeSink,
}

impl<'a> CtxParts<'a> {
    fn ctx<'b>(&'b self, sender: &'b MeshSender) -> HandlerCtx<'b>
    where
        'a: 'b,
    {
        HandlerCtx {
            sender,
            endpoint: self.endpoint,
            mesh: self.mesh,
            author: self.author,
            identity: self.identity,
            our_pubkey: self.our_pubkey,
            max_peers: self.max_peers,
            rendezvous_id: self.rendezvous_id,
            external_msg_tx: self.external_msg_tx,
            sink: self.sink,
        }
    }
}

/// Outcome of one resubscribe attempt (the heal arm drives one per
/// tick while the gossip stream is down).
enum Resubscribe {
    Restored(GossipSender, GossipReceiver),
    Pending,
    Fatal,
}

/// The resubscribe attempt's read-only environment: the gossip frontend to
/// re-subscribe through, the rendezvous params to bootstrap from, the shared
/// ctx-building parts (for its `sink` and to rebuild a [`HandlerCtx`] once the
/// sender is swapped), and the CLI hard-exit flag for the `Fatal` path.
struct ResubscribeEnv<'a> {
    gossip: &'a iroh_gossip::net::Gossip,
    params: &'a beacon::RendezvousParams,
    parts: &'a CtxParts<'a>,
    exit_on_quit: bool,
}

/// The live gossip link a resubscribe mutates: the sender/receiver pair
/// swapped in on success, and the consecutive-failure counter it resets or
/// increments.
struct GossipLink<'a> {
    sender: &'a mut MeshSender,
    receiver: &'a mut GossipReceiver,
    attempts: &'a mut u32,
}

/// One heal-tick turn while the gossip stream is down: attempt the
/// resubscribe and, on success, swap in the fresh sender/receiver,
/// drain the dead subscription's buffer (the actor counts those
/// messages as delivered — overlay dedup will never re-push them, and
/// anti-entropy resends of them are deduped too, so the buffer is the
/// only copy), then re-enter the overlay via the starvation-recovery
/// primitive (degraded mesh, throttles cleared, known peers re-dialed,
/// arrival re-announced). On `Fatal` (the actor itself is gone) the
/// daemon stops posing as a live member: statusline state file cleared
/// (a `Left` broadcast is pointless on a dead topic), `exit(1)` on the
/// CLI path, `Err` for embedded drivers.
async fn resubscribe_tick(
    env: &ResubscribeEnv<'_>,
    state: &mut EventLoopState,
    app: &mut dyn NodeApp,
    link: GossipLink<'_>,
) -> Result<()> {
    match try_resubscribe(env, state, link.attempts).await {
        Resubscribe::Restored(new_sender, new_receiver) => {
            let mut dead_receiver = std::mem::replace(link.receiver, new_receiver);
            // Swap only the inner gossip sender; the spool writer Arc survives
            // the resubscribe so a healed daemon keeps mirroring.
            link.sender.replace_gossip(new_sender);
            state.gossip_open = true;
            // The dead subscription's link view is void; the fresh one
            // emits its own NeighborUps (and re-arms the probe gate).
            state.rendezvous_linked = false;
            let ctx = env.parts.ctx(link.sender);
            gossip::drain_dead_receiver(&mut dead_receiver, state, app, &ctx).await;
            drop(dead_receiver);
            gossip::heal::recover_from_starvation(state, &ctx).await;
        }
        Resubscribe::Pending => {}
        Resubscribe::Fatal => {
            if let Some(state_file) = state.state_file.as_ref() {
                state_file.remove();
            }
            env.parts.sink.emit(NodeEvent::Error(
                "gossip subscription unrecoverable; shutting down".to_owned(),
            ));
            #[cfg(not(feature = "dhat-heap"))]
            if env.exit_on_quit {
                std::process::exit(1);
            }
            #[cfg(feature = "dhat-heap")]
            let _ = env.exit_on_quit;
            anyhow::bail!("gossip subscription unrecoverable after repeated resubscribe attempts");
        }
    }
    Ok(())
}

/// Re-open the gossip topic after its stream terminally ended. The
/// designed-for remedy, not a workaround: iroh-gossip closes a lagging
/// subscriber outright and its docs instruct "close and re-open".
/// Bootstrap is the rendezvous plus every remembered peer so the fresh
/// subscription re-grafts without waiting for lookups. `Fatal` after
/// `RESUBSCRIBE_MAX_ATTEMPTS` consecutive failures: a subscribe error
/// means the gossip actor itself is gone (endpoint closed), which no
/// retry can fix.
async fn try_resubscribe(
    env: &ResubscribeEnv<'_>,
    state: &EventLoopState,
    attempts: &mut u32,
) -> Resubscribe {
    let mut bootstrap = vec![env.params.id];
    bootstrap.extend(state.known_endpoints.iter().copied());
    match env.gossip.subscribe(env.params.topic_id, bootstrap).await {
        Ok(topic) => {
            *attempts = 0;
            tracing::warn!(
                target: "agent_square::gossip",
                "gossip stream restored (resubscribed)"
            );
            env.parts.sink.emit(NodeEvent::Info(
                "gossip stream restored; rejoining the mesh".to_owned(),
            ));
            let (sender, receiver) = topic.split();
            Resubscribe::Restored(sender, receiver)
        }
        Err(error) => {
            *attempts += 1;
            tracing::warn!(
                target: "agent_square::gossip",
                %error,
                attempts = *attempts,
                "gossip resubscribe failed"
            );
            if *attempts >= RESUBSCRIBE_MAX_ATTEMPTS {
                Resubscribe::Fatal
            } else {
                Resubscribe::Pending
            }
        }
    }
}

/// Apply a bootstrap rung chosen **off the event loop** (the startup
/// confirmation probe or the beacon's liveness self-monitor publishing
/// through `rendezvous_params.rung_tx`). Cheap and non-blocking — the
/// ladder walk already ran in the background task. If the new rung
/// differs from the one we're homed on, re-pre-register `rendezvous_id`
/// at it and drop the beacon so `maybe_cohost` rebuilds it homed on the
/// new rung.
fn apply_rung_change(
    params: &mut beacon::RendezvousParams,
    endpoint: &Endpoint,
    rendezvous: &mut Option<beacon::Rendezvous>,
    rung_rx: &watch::Receiver<Option<RelayUrl>>,
) {
    let selected = rung_rx.borrow().clone();
    if let lookup::RungRefresh::Rehome(new) =
        lookup::plan_rung_refresh(params.bootstrap_relay.as_ref(), selected)
    {
        tracing::info!(
            target: "agent_square::beacon",
            old = ?params.bootstrap_relay,
            new = ?new,
            "bootstrap relay rung changed; re-registering rendezvous and re-homing the beacon"
        );
        params.bootstrap_relay = new;
        setup::register_rendezvous(endpoint, params);
        // Drop the beacon: `maybe_cohost` → `beacon::ensure` rebuilds it
        // homed on the new rung at the next heal/reclaim tick.
        *rendezvous = None;
    }
}

/// One stdin-line read; returns the new `stdin_open` (`false` on EOF/error).
/// The line itself is broadcast by the app ([`NodeDriver::handle_stdin`]).
async fn handle_stdin_arm<A: NodeDriver>(
    result: std::io::Result<LineRead>,
    app: &mut A,
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
) -> bool {
    match result {
        Err(_) | Ok(LineRead::Eof) => false,
        Ok(LineRead::TooLong) => {
            ctx.sink
                .emit(NodeEvent::Error("input line too long; ignored".to_owned()));
            true
        }
        Ok(LineRead::Line(line)) => {
            app.handle_stdin(line.trim(), state, ctx).await;
            true
        }
    }
}

/// The co-host decision inputs a heal/reclaim tick needs: which policy
/// governs this session, the rendezvous params to (re)claim under, and
/// (for the unmeshed-joiner grace `maybe_cohost` alone reads) when the
/// event loop started.
struct CohostArm<'a> {
    policy: CoHostPolicy,
    params: &'a beacon::RendezvousParams,
    started: Instant,
}

/// Heal-tick co-host: stand up the beacon if this member may serve it
/// now (`may_cohost`). Claim-if-free in private; in public a non-`Eager`
/// member probes first (`beacon::ensure`) so it never registers a
/// duplicate rendezvous that would capture its own bootstrap dial.
async fn maybe_cohost(
    state: &EventLoopState,
    ctx: &HandlerCtx<'_>,
    arm: &CohostArm<'_>,
    current: &mut Option<beacon::Rendezvous>,
) {
    if may_cohost(arm.policy, state.meshed, arm.started) {
        beacon::ensure(
            arm.params,
            ctx.endpoint,
            current,
            probes_before_claim(arm.policy),
        )
        .await;
    }
}

/// Fast event-driven failover: while the post-`NeighborDown` reclaim
/// window is open, retry the rendezvous claim so a survivor takes the
/// freed port in ~1s instead of waiting for the 15s heal tick. A no-op
/// outside the window (just an `Instant` compare) and idempotent once
/// the rendezvous is held. `Never` consumers never reclaim; everyone
/// else probes first (`!Eager`) so a survivor that already took over
/// isn't displaced by a colliding duplicate.
async fn maybe_reclaim(
    state: &EventLoopState,
    ctx: &HandlerCtx<'_>,
    arm: &CohostArm<'_>,
    current: &mut Option<beacon::Rendezvous>,
) {
    if arm.policy != CoHostPolicy::Never
        && state
            .reclaim_until
            .is_some_and(|deadline| Instant::now() < deadline)
    {
        beacon::ensure(
            arm.params,
            ctx.endpoint,
            current,
            probes_before_claim(arm.policy),
        )
        .await;
    }
}

/// The time-driven maintenance tickers.
struct MaintenanceIntervals {
    prune: tokio::time::Interval,
    alive: tokio::time::Interval,
    sweep: tokio::time::Interval,
    heal: tokio::time::Interval,
    /// Fast event-driven failover burst; only does work while
    /// `state.reclaim_until` is open (armed on `NeighborDown`).
    reclaim: tokio::time::Interval,
    /// Periodic anti-entropy digest broadcast (recover messages missed
    /// while partitioned/asleep).
    antientropy: tokio::time::Interval,
    state_refresh: tokio::time::Interval,
    /// Periodic relay link-state re-broadcast (our own measured links), so peers
    /// keep a fresh routing graph.
    linkstate: tokio::time::Interval,
}

/// Build the maintenance tickers, eating the first immediate tick on
/// the ones that must wait a full period (we just announced `Joined`).
///
/// Every ticker uses [`MissedTickBehavior::Skip`]: after the monotonic clock
/// jumps (an App Nap throttle, a SIGSTOP freeze), a default `Burst` ticker
/// fires a catch-up salvo — several anti-entropy digests back-to-back, a
/// heal immediately after the hard re-bootstrap it just ran, a prune replaying
/// its backlog — and poisons the tick-gap telemetry (the burst ticks report a
/// ~0 gap). Each tick here means "do the maintenance now", so a skipped tick is
/// free; `Skip` collapses the salvo to one tick on the next aligned boundary.
async fn build_maintenance_intervals() -> MaintenanceIntervals {
    use tokio::time::MissedTickBehavior::Skip;

    let mut prune = tokio::time::interval(Duration::from_mins(1));
    prune.set_missed_tick_behavior(Skip);
    let mut alive = tokio::time::interval(Duration::from_secs(ALIVE_INTERVAL_SECS));
    alive.set_missed_tick_behavior(Skip);
    alive.tick().await;
    let mut sweep = tokio::time::interval(Duration::from_secs(sweep_interval_secs()));
    sweep.set_missed_tick_behavior(Skip);
    sweep.tick().await;
    let mut heal = tokio::time::interval(Duration::from_secs(heal_interval_secs()));
    heal.set_missed_tick_behavior(Skip);
    heal.tick().await;
    let mut reclaim = tokio::time::interval(Duration::from_millis(RECLAIM_INTERVAL_MS));
    reclaim.set_missed_tick_behavior(Skip);
    reclaim.tick().await;
    let mut antientropy = tokio::time::interval(Duration::from_secs(antientropy_interval_secs()));
    antientropy.set_missed_tick_behavior(Skip);
    antientropy.tick().await;
    let mut state_refresh = tokio::time::interval(Duration::from_secs(STATE_REFRESH_SECS));
    state_refresh.set_missed_tick_behavior(Skip);
    state_refresh.tick().await;
    let mut linkstate = tokio::time::interval(Duration::from_secs(LINKSTATE_INTERVAL_SECS));
    linkstate.set_missed_tick_behavior(Skip);
    linkstate.tick().await;
    MaintenanceIntervals {
        prune,
        alive,
        sweep,
        heal,
        reclaim,
        antientropy,
        state_refresh,
        linkstate,
    }
}

/// `select!`-friendly receive over an optional mpsc channel: yields
/// the next item, `None` once the channel closes, and pends forever
/// when the channel is absent so the arm stays inert for drivers that
/// don't wire it (CLI/MCP). Shared by the ipc / external-quit /
/// external-send arms so the idiom isn't written three ways.
async fn recv_opt<T>(rx: &mut Option<mpsc::Receiver<T>>) -> Option<T> {
    match rx.as_mut() {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        CoHostPolicy, claims_at_startup, is_resume, is_wall_resume, orphan_watch_warranted,
        parent_lost, probes_before_claim,
    };

    #[test]
    fn directory_advertiser_claims_at_startup_with_probe() {
        // Regression for the duplicate-beacon directory bug: an advertiser
        // must co-host the shared rendezvous from t=0 *and* probe-first, so a
        // second advertiser into the same directory defers instead of binding
        // a duplicate (which partitioned the directory in public mode — only
        // one mesh was discoverable). The pre-fix policy was the no-probe
        // `Eager` (claims, doesn't probe), which the probe assertion guards.
        let advertiser = crate::daemon::config::DIRECTORY_ADVERTISER_COHOST;
        assert!(claims_at_startup(advertiser), "must claim at t=0");
        assert!(
            probes_before_claim(advertiser),
            "must probe before claiming"
        );

        // The mesh origin (`create`) claims at startup but skips the probe;
        // joiners and consumers don't claim at startup at all.
        assert!(claims_at_startup(CoHostPolicy::Eager));
        assert!(!probes_before_claim(CoHostPolicy::Eager));
        assert!(!claims_at_startup(CoHostPolicy::Deferred));
        assert!(!claims_at_startup(CoHostPolicy::Never));
    }

    #[test]
    fn orphan_watch_fires_only_on_a_parent_change() {
        // The agent that spawned us is alive ⇒ same ppid ⇒ stay running.
        assert!(!parent_lost(4242, 4242));
        // The agent died ⇒ reparented to launchd (1) ⇒ orphaned, quit.
        assert!(parent_lost(4242, 1));
        // …or, under a systemd subreaper, to some other pid ⇒ still orphaned.
        assert!(parent_lost(4242, 990));
    }

    #[test]
    fn orphan_watch_skips_an_already_detached_daemon() {
        // Spawned by a normal agent ⇒ worth watching.
        assert!(orphan_watch_warranted(4242));
        // Launched detached straight from init/launchd ⇒ no parent to lose.
        assert!(!orphan_watch_warranted(1));
    }

    #[test]
    fn is_resume_only_past_threshold() {
        let threshold = Duration::from_mins(1);
        // A normal heal cadence (≤ ~15s) is never a resume.
        assert!(!is_resume(Duration::from_secs(0), threshold));
        assert!(!is_resume(Duration::from_secs(15), threshold));
        assert!(!is_resume(Duration::from_secs(59), threshold));
        // Exactly at the threshold is not yet a stall (strictly `>`).
        assert!(!is_resume(Duration::from_mins(1), threshold));
        // A multi-minute gap = the process was frozen → hard re-bootstrap.
        assert!(is_resume(Duration::from_secs(61), threshold));
        assert!(is_resume(Duration::from_hours(1), threshold));
    }

    #[test]
    fn is_resume_respects_injected_threshold() {
        // The subprocess stall regression shortens the threshold via
        // the env knob; the comparison must track whatever is passed.
        let short = Duration::from_secs(4);
        assert!(!is_resume(Duration::from_secs(3), short));
        assert!(is_resume(Duration::from_secs(5), short));
    }

    #[test]
    fn wall_resume_detects_macos_sleep_signature() {
        let threshold = Duration::from_mins(1);
        // macOS sleep: the monotonic clock froze (a few seconds of
        // real post-wake time) while the wall clock jumped a full day.
        // The monotonic gap alone misses it; the divergence catches it.
        let mono_gap = Duration::from_secs(3);
        let wall_gap = Duration::from_hours(24);
        assert!(!is_resume(mono_gap, threshold));
        assert!(is_wall_resume(wall_gap, mono_gap, threshold));
    }

    #[test]
    fn wall_resume_ignores_clocks_advancing_together() {
        let threshold = Duration::from_mins(1);
        // Steady operation: wall and monotonic advance in lockstep, so
        // their divergence is ~0 — never a resume, whatever the cadence.
        assert!(!is_wall_resume(
            Duration::from_secs(15),
            Duration::from_secs(15),
            threshold
        ));
        // A wall clock running slightly behind monotonic (NTP step
        // back) saturates to 0 divergence, not a spurious resume.
        assert!(!is_wall_resume(
            Duration::from_secs(10),
            Duration::from_secs(15),
            threshold
        ));
    }
}
