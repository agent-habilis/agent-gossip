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
use iroh::{Endpoint, RelayUrl};
use iroh_gossip::api::{GossipReceiver, GossipSender};
use tokio::io::BufReader;
use tokio::sync::{broadcast, mpsc, watch};

use crate::daemon::state_file::StateFile;
use crate::output;
use crate::protocol::swarm::SwarmName;
use crate::protocol::{Message, Nickname, SwarmId};
use crate::util::bounded_read::{LineRead, read_bounded_line};
use crate::{beacon, gossip, lifecycle, lookup};
use ahs_shared::MAX_STDIN_LINE_BYTES;
// Bare `ipc` is `daemon::ipc`; transport's socket server is by-item.
use crate::transport::ipc::{IpcMessage, listen};
use crate::util::tuning::{
    ALIVE_INTERVAL_SECS, ANTIENTROPY_INTERVAL_SECS, HEAL_INTERVAL_SECS, RECLAIM_INTERVAL_MS,
    STATE_REFRESH_SECS, heal_stall_threshold_secs, sweep_interval_secs,
};

use super::config::{CoHostPolicy, DriverMode, EventLoopConfig, SessionRequest};
use super::ctx::HandlerCtx;
use super::state::EventLoopState;
use super::{ipc, setup, timers};

/// Never returns normally — exits the process on ctrl-c / SIGTERM.
pub(crate) async fn run(cfg: EventLoopConfig) -> Result<()> {
    let EventLoopConfig {
        topic,
        author,
        swarm: swarm_str,
        name: swarm_name,
        output,
        interactive,
        endpoint,
        router: _router,
        max_peers,
        rate_limit_per_min,
        rendezvous_params,
        rung_rx,
        cohost,
        state_file,
        live_count,
        driver,
    } = cfg;

    // Every driver-derived fact in one place. Only the CLI exits the
    // process on quit and binds the unix socket; in-process drivers
    // (embed / MCP) take typed requests on `req_rx` instead.
    let (external_quit_rx, external_msg_tx, external_req_rx, ipc_listener_disabled, exit_on_quit) =
        match driver {
            DriverMode::Cli => (None, None, None, false, true),
            DriverMode::InProcess {
                msg_tx,
                req_rx,
                quit_rx,
            } => (Some(quit_rx), msg_tx, Some(req_rx), true, false),
        };

    let started = Instant::now();
    let state_file = state_file.map(|path| StateFile::new(path, &swarm_str, &author, &swarm_name));
    let mut state = EventLoopState::new(state_file, started, rate_limit_per_min);
    // Advertise path only: the directory re-broadcast task reads the
    // live count from here. Set before the first write below so the
    // initial ad carries a real count.
    state.live_count = live_count;
    state.write_participant_count();

    // An eager member co-hosts from t=0 so a beacon exists before any
    // joiner subscribes; everyone else defers to the heal gate
    // (`may_cohost`). `Eager` skips the probe (a brand-new swarm has no
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

    let (sender, receiver) = topic.split();

    let ipc_rx = spawn_ipc_rx(
        ipc_listener_disabled,
        &swarm_str,
        &author,
        // The IPC listener runs as its own task; hand it an owned
        // clone (cheap — `Capture` is an `Arc`-backed sender).
        output.clone(),
    );

    // Arrival announce is deferred to the first `NeighborUp` — see
    // `gossip::handle_gossip_event`.

    let intervals = build_maintenance_intervals().await;
    let quit_rx = spawn_quit_signal_tasks();

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
        endpoint,
        swarm: swarm_str,
        name: swarm_name,
        author,
        output,
        max_peers,
        state,
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
    }))
    .await
}

/// Owned working set for [`event_loop`]. `run` does setup, fills this,
/// and hands it over; the loop destructures it back into the same
/// locals the orchestrator used to hold inline. Splitting the 11-arm
/// `select!` out keeps both functions within the readability budget
/// (clippy `too_many_lines`) without an `#[allow]`.
struct EventLoop {
    sender: GossipSender,
    receiver: GossipReceiver,
    endpoint: Endpoint,
    swarm: SwarmId,
    name: SwarmName,
    author: Nickname,
    output: output::Output,
    max_peers: usize,
    state: EventLoopState,
    ipc_rx: Option<mpsc::Receiver<IpcMessage>>,
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
    external_req_rx: Option<mpsc::Receiver<SessionRequest>>,
    external_msg_tx: Option<broadcast::Sender<Message>>,
    quit_rx: mpsc::Receiver<()>,
    exit_on_quit: bool,
}

/// The daemon's `select!` loop. Never returns normally on the CLI
/// path (ctrl-c / SIGTERM `std::process::exit`s); embedded drivers
/// break out via their external quit channel and get `Ok(())`.
async fn event_loop(loop_state: EventLoop) -> Result<()> {
    let EventLoop {
        sender,
        mut receiver,
        endpoint,
        swarm: swarm_str,
        name: swarm_name,
        author,
        output,
        max_peers,
        mut state,
        mut ipc_rx,
        interactive,
        intervals,
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
    } = loop_state;

    let MaintenanceIntervals {
        prune: mut prune_interval,
        alive: mut alive_interval,
        sweep: mut sweep_interval,
        heal: mut heal_interval,
        reclaim: mut reclaim_interval,
        antientropy: mut antientropy_interval,
        state_refresh: mut state_refresh_interval,
    } = intervals;

    let mut stdin_reader = BufReader::new(tokio::io::stdin());
    let mut stdin_open = interactive;

    // Per-timer gap trackers; the heal gap also drives the
    // resume-edge hard re-bootstrap. Each timer carries a monotonic
    // anchor AND a wall-clock anchor: on macOS the monotonic clock
    // pauses in lockstep with a sleeping process, so only the wall gap
    // reveals a suspend (see `note_tick_gap` / `run_heal`).
    let wall_now = crate::util::clock::unix_secs();
    let mut last_alive = Instant::now();
    let mut last_sweep = Instant::now();
    let mut last_heal = Instant::now();
    let mut last_antientropy = Instant::now();
    let mut last_alive_wall = wall_now;
    let mut last_sweep_wall = wall_now;
    let mut last_heal_wall = wall_now;
    let mut last_antientropy_wall = wall_now;

    let ctx = HandlerCtx {
        sender: &sender,
        endpoint: &endpoint,
        swarm: &swarm_str,
        author: &author,
        max_peers,
        rendezvous_id: rendezvous_params.id,
        external_msg_tx: external_msg_tx.as_ref(),
        output: &output,
    };

    loop {
        let ping_deadline = state.ping_round.as_ref().map(|round| round.deadline);
        tokio::select! {
            result = read_bounded_line(&mut stdin_reader, MAX_STDIN_LINE_BYTES), if stdin_open => {
                stdin_open = handle_stdin_arm(result, &sender, &swarm_str, &author, &mut state, &output).await;
            }
            () = sleep_until_opt(ping_deadline) => {
                finalize_ping_round(&mut state, &output);
            }
            ipc_msg = recv_opt(&mut ipc_rx) => {
                if !handle_ipc_arm(ipc_msg, &swarm_str, &author, &mut state, &sender, &output).await {
                    ipc_rx = None;
                }
            }
            event = receiver.next(), if state.gossip_open => {
                gossip::handle_gossip_event(event, &mut state, &ctx).await;
            }
            _ = prune_interval.tick() => timers::tick_prune(&mut state),
            _ = alive_interval.tick() => {
                timers::note_tick_gap("alive", &mut last_alive, &mut last_alive_wall, Duration::from_secs(ALIVE_INTERVAL_SECS));
                lifecycle::heartbeat::tick_alive(&mut state, &sender, &swarm_str, &author).await;
            }
            _ = sweep_interval.tick() => {
                timers::note_tick_gap("sweep", &mut last_sweep, &mut last_sweep_wall, Duration::from_secs(sweep_interval_secs()));
                lifecycle::heartbeat::tick_sweep(&mut state, &output);
            }
            _ = heal_interval.tick() => {
                let (mono_gap, wall_gap) = timers::note_tick_gap("heal", &mut last_heal, &mut last_heal_wall, Duration::from_secs(HEAL_INTERVAL_SECS));
                heal_tick(mono_gap, wall_gap, &mut state, &endpoint, &sender, &rendezvous_params, cohost, started, &mut rendezvous).await;
            }
            // A bootstrap rung chosen off-loop (startup probe / beacon self-monitor); apply it cheaply.
            // `Ok(())` only: a closed channel (impossible while the beacon params live) disables the arm.
            Ok(()) = rung_rx.changed() => apply_rung_change(&mut rendezvous_params, &endpoint, &mut rendezvous, &rung_rx),
            _ = reclaim_interval.tick() => {
                maybe_reclaim(cohost, &state, &rendezvous_params, &endpoint, &mut rendezvous).await;
            }
            _ = antientropy_interval.tick() => {
                timers::note_tick_gap("antientropy", &mut last_antientropy, &mut last_antientropy_wall, Duration::from_secs(ANTIENTROPY_INTERVAL_SECS));
                gossip::antientropy::broadcast_digest(&mut state, &sender, &swarm_str, &author).await;
            }
            _ = state_refresh_interval.tick() => timers::tick_state_refresh(&state, &endpoint).await,
            _ = recv_opt(&mut external_quit_rx) => {
                shutdown(&sender, &swarm_str, &swarm_name, &author, &state, &output).await;
                // External quit is always embedded (MCP): never exit
                // the process, regardless of `exit_on_quit`.
                break;
            }
            req = recv_opt(&mut external_req_rx) => {
                match req {
                    Some(req) => {
                        if gossip::handle_session_request(req, &swarm_str, &author, &mut state, &sender, &output).await {
                            state.last_sent_at = Instant::now();
                        }
                    }
                    None => external_req_rx = None,
                }
            }
            _ = quit_rx.recv() => {
                shutdown(&sender, &swarm_str, &swarm_name, &author, &state, &output).await;
                if exit_on_quit {
                    // Force exit — the blocking stdin thread won't
                    // terminate on its own. CLI mode.
                    std::process::exit(0);
                }
                break;
            }
        }
    }

    Ok(())
}

/// Graceful shutdown: announce `Left`, give the broadcast a moment
/// to reach peers, then remove the statusline state file. Shared
/// by both the external-quit and ctrl-c/SIGTERM paths so they
/// can't drift apart.
async fn shutdown(
    sender: &GossipSender,
    swarm: &SwarmId,
    name: &SwarmName,
    author: &Nickname,
    state: &EventLoopState,
    output: &output::Output,
) {
    output.info(&format!("left #{name}"));
    lifecycle::log_leaving(name.as_str());
    gossip::broadcast_msg(sender, &Message::new_left(swarm, author)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    if let Some(sf) = state.state_file.as_ref() {
        sf.remove();
    }
}

/// Spawn ctrl-c (all platforms) and SIGTERM (unix) listener tasks
/// feeding a single internal quit channel. `tokio::signal::ctrl_c()`
/// inside a `select!` branch doesn't reliably interrupt a blocking
/// stdin read, so we offload signal listening to dedicated tasks.
fn spawn_quit_signal_tasks() -> mpsc::Receiver<()> {
    let (quit_tx, quit_rx) = mpsc::channel::<()>(1);
    let quit_tx2 = quit_tx.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = quit_tx.send(()).await;
    });
    #[cfg(unix)]
    tokio::spawn(async move {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to register SIGTERM handler");
        sigterm.recv().await;
        let _ = quit_tx2.send(()).await;
    });
    quit_rx
}

/// Resolve the IPC receiver: reuse a pre-wired channel (MCP/embed) or,
/// for the CLI, spawn the unix-socket listener and own the channel.
/// Returning `Option` keeps the loop's `select!` arm uniform.
fn spawn_ipc_rx(
    disable_ipc_listener: bool,
    swarm: &SwarmId,
    author: &Nickname,
    output: output::Output,
) -> Option<mpsc::Receiver<IpcMessage>> {
    // In-process mode (embed / MCP): no socket. Returning `None` leaves
    // the loop's IPC `select!` arm inert (it pends forever), so the
    // unix-socket listener is never bound — those drivers use the typed
    // `req_rx` instead.
    if disable_ipc_listener {
        return None;
    }
    let (ipc_tx, rx) = mpsc::channel::<IpcMessage>(32);
    tokio::spawn(listen(swarm.clone(), author.clone(), ipc_tx, output));
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
fn finalize_ping_round(state: &mut EventLoopState, output: &output::Output) {
    let Some(round) = state.ping_round.take() else {
        return;
    };
    let mut peers: Vec<output::PingPeer> = round
        .pongs
        .iter()
        .map(|(nickname, arrival)| output::PingPeer {
            nickname: nickname.clone(),
            rtt_ms: u64::try_from(arrival.duration_since(round.t1).as_millis()).unwrap_or(u64::MAX),
        })
        .collect();
    peers.sort_by(|left, right| left.nickname.as_str().cmp(right.nickname.as_str()));
    output.ping_report(peers, state.participants.len());
}

/// Whether a co-hosting member probes the rendezvous before claiming it —
/// the single source of truth for the `probe_first` flag passed to
/// [`beacon::ensure`] from every claim site (startup, heal tick, reclaim
/// window). Only `Eager` (the swarm origin) skips the probe: a brand-new
/// swarm has no peers to self-collide with. Every other policy probes, so
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
/// `cohost_grace_secs` for an empty swarm (then probe-gated in
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
    mono_gap: Duration,
    wall_gap: Duration,
    state: &mut EventLoopState,
    endpoint: &Endpoint,
    sender: &GossipSender,
    params: &beacon::RendezvousParams,
) {
    let threshold = Duration::from_secs(heal_stall_threshold_secs());
    let hard_edge = is_resume(mono_gap, threshold) || is_wall_resume(wall_gap, mono_gap, threshold);
    if hard_edge {
        tracing::warn!(
            target: "agent_habilis_swarm::gossip",
            mono_gap_ms = u64::try_from(mono_gap.as_millis()).unwrap_or(u64::MAX),
            wall_gap_ms = u64::try_from(wall_gap.as_millis()).unwrap_or(u64::MAX),
            "heal: hard re-bootstrap edge"
        );
        state.meshed = false;
        // Re-assert the rendezvous hint (the network changed). The rung
        // is re-validated off-loop by the beacon's liveness self-monitor,
        // so a rung that died during the freeze self-corrects — no inline
        // ladder walk on the event loop here.
        setup::register_rendezvous(endpoint, params);
        gossip::heal::tick_heal_hard(endpoint, params.id, sender).await;
    } else {
        gossip::heal::tick_heal(endpoint, params.id, sender).await;
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
        gossip::heal::rebridge_known(sender, &state.known_endpoints).await;
    }
}

/// One heal tick: re-bootstrap/heal, then (re)claim the beacon if we
/// should co-host. Grouped so the event-loop arm stays a one-liner.
#[expect(
    clippy::too_many_arguments,
    reason = "threads the heal + cohost state the event loop owns; splitting would only re-bundle it"
)]
async fn heal_tick(
    mono_gap: Duration,
    wall_gap: Duration,
    state: &mut EventLoopState,
    endpoint: &Endpoint,
    sender: &GossipSender,
    params: &beacon::RendezvousParams,
    cohost: CoHostPolicy,
    started: Instant,
    rendezvous: &mut Option<beacon::Rendezvous>,
) {
    run_heal(mono_gap, wall_gap, state, endpoint, sender, params).await;
    maybe_cohost(cohost, state, started, params, endpoint, rendezvous).await;
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
            target: "agent_habilis_swarm::beacon",
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

/// One stdin-line read; returns the new `stdin_open` (`false` on
/// EOF/error). Split out of `event_loop` for the line budget.
async fn handle_stdin_arm(
    result: std::io::Result<LineRead>,
    sender: &GossipSender,
    swarm: &SwarmId,
    author: &Nickname,
    state: &mut EventLoopState,
    output: &output::Output,
) -> bool {
    match result {
        Err(_) | Ok(LineRead::Eof) => false,
        Ok(LineRead::TooLong) => {
            output.error("input line too long; ignored");
            true
        }
        Ok(LineRead::Line(line)) => {
            gossip::handle_stdin_line(line.trim(), sender, swarm, author, state, output).await;
            true
        }
    }
}

/// One IPC command; returns `false` when the channel has closed (the
/// caller then drops its receiver). Split out of `event_loop` for the
/// line budget.
async fn handle_ipc_arm(
    ipc_msg: Option<IpcMessage>,
    swarm: &SwarmId,
    author: &Nickname,
    state: &mut EventLoopState,
    sender: &GossipSender,
    output: &output::Output,
) -> bool {
    match ipc_msg {
        None => false,
        Some((cmd, resp_tx)) => {
            if ipc::handle_ipc_command(cmd, resp_tx, swarm, author, state, sender, output).await {
                state.last_sent_at = Instant::now();
            }
            true
        }
    }
}

/// Heal-tick co-host: stand up the beacon if this member may serve it
/// now (`may_cohost`). Claim-if-free in private; in public a non-`Eager`
/// member probes first (`beacon::ensure`) so it never registers a
/// duplicate rendezvous that would capture its own bootstrap dial.
async fn maybe_cohost(
    cohost: CoHostPolicy,
    state: &EventLoopState,
    started: Instant,
    params: &beacon::RendezvousParams,
    endpoint: &Endpoint,
    current: &mut Option<beacon::Rendezvous>,
) {
    if may_cohost(cohost, state.meshed, started) {
        beacon::ensure(params, endpoint, current, probes_before_claim(cohost)).await;
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
    cohost: CoHostPolicy,
    state: &EventLoopState,
    params: &beacon::RendezvousParams,
    endpoint: &Endpoint,
    current: &mut Option<beacon::Rendezvous>,
) {
    if cohost != CoHostPolicy::Never
        && state
            .reclaim_until
            .is_some_and(|deadline| Instant::now() < deadline)
    {
        beacon::ensure(params, endpoint, current, probes_before_claim(cohost)).await;
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
}

/// Build the maintenance tickers, eating the first immediate tick on
/// the ones that must wait a full period (we just announced `Joined`).
async fn build_maintenance_intervals() -> MaintenanceIntervals {
    let prune = tokio::time::interval(Duration::from_mins(1));
    let mut alive = tokio::time::interval(Duration::from_secs(ALIVE_INTERVAL_SECS));
    alive.tick().await;
    let mut sweep = tokio::time::interval(Duration::from_secs(sweep_interval_secs()));
    sweep.tick().await;
    let mut heal = tokio::time::interval(Duration::from_secs(HEAL_INTERVAL_SECS));
    heal.tick().await;
    let mut reclaim = tokio::time::interval(Duration::from_millis(RECLAIM_INTERVAL_MS));
    reclaim.tick().await;
    let mut antientropy = tokio::time::interval(Duration::from_secs(ANTIENTROPY_INTERVAL_SECS));
    antientropy.tick().await;
    let mut state_refresh = tokio::time::interval(Duration::from_secs(STATE_REFRESH_SECS));
    state_refresh.tick().await;
    MaintenanceIntervals {
        prune,
        alive,
        sweep,
        heal,
        reclaim,
        antientropy,
        state_refresh,
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

    use super::{CoHostPolicy, claims_at_startup, is_resume, is_wall_resume, probes_before_claim};

    #[test]
    fn directory_advertiser_claims_at_startup_with_probe() {
        // Regression for the duplicate-beacon directory bug: an advertiser
        // must co-host the shared rendezvous from t=0 *and* probe-first, so a
        // second advertiser into the same directory defers instead of binding
        // a duplicate (which partitioned the directory in public mode — only
        // one swarm was discoverable). The pre-fix policy was the no-probe
        // `Eager` (claims, doesn't probe), which the probe assertion guards.
        let advertiser = crate::embed::DIRECTORY_ADVERTISER_COHOST;
        assert!(claims_at_startup(advertiser), "must claim at t=0");
        assert!(
            probes_before_claim(advertiser),
            "must probe before claiming"
        );

        // The swarm origin (`create`) claims at startup but skips the probe;
        // joiners and consumers don't claim at startup at all.
        assert!(claims_at_startup(CoHostPolicy::Eager));
        assert!(!probes_before_claim(CoHostPolicy::Eager));
        assert!(!claims_at_startup(CoHostPolicy::Deferred));
        assert!(!claims_at_startup(CoHostPolicy::Never));
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
