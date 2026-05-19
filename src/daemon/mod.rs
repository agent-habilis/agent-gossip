//! The daemon's shared event loop — used by both `create` and `join`.
//!
//! The loop owns three kinds of work:
//!
//! - **External inputs**: stdin (interactive mode), IPC commands
//!   (`msg` / `poll`), and incoming gossip events.
//! - **Time-driven maintenance**: heartbeat keepalives, silence
//!   sweeps, gossip healer, rate-limit pruning.
//! - **Shutdown**: ctrl-c / SIGTERM.
//!
//! `daemon` is orchestration + plumbing: the `select!` loop, IPC
//! command application (`ipc`), shared handler context (`ctx`),
//! in-memory accounting (`state`, `message_log`, `rate_limit`),
//! `config`, `setup`, housekeeping `timers`. The behavioral
//! subsystems are crate-root siblings, each its own `RUST_LOG`
//! target: `crate::gossip`, `crate::lifecycle`, `crate::beacon`,
//! `crate::discovery`.

mod config;
pub(crate) mod ctx;
pub(crate) mod ipc;
// In-memory accounting stores owned by `EventLoopState`. Private to
// `daemon` — no consumer outside the event loop.
mod message_log;
mod rate_limit;
pub(crate) mod setup;
pub(crate) mod state;
pub(crate) mod timers;

use std::time::{Duration, Instant};

use anyhow::Result;
use futures_util::StreamExt;
use iroh::Endpoint;
use iroh_gossip::api::{GossipReceiver, GossipSender};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{broadcast, mpsc};

use crate::output;
use crate::protocol::swarm::SwarmName;
use crate::protocol::{Message, Nickname, SwarmId};
use crate::util::state_file::StateFile;
use crate::{beacon, gossip, lifecycle};
// Bare `ipc` is `daemon::ipc`; transport's socket server is by-item.
use crate::transport::ipc::{IpcMessage, listen};
use crate::util::tuning::{
    ALIVE_INTERVAL_SECS, ANTIENTROPY_INTERVAL_SECS, HEAL_INTERVAL_SECS, RECLAIM_INTERVAL_MS,
    STATE_REFRESH_SECS, sweep_interval_secs,
};

use ctx::HandlerCtx;
use state::EventLoopState;

pub(crate) use config::{DriverMode, EventLoopConfig, SendRequest};

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
        rendezvous_params,
        co_host_eagerly,
        state_file,
        driver,
    } = cfg;

    // Every driver-derived fact in one place. Only the CLI exits the
    // process on quit; only the CLI binds the unix socket (MCP has a
    // pre-wired `ipc_rx`, embed has neither).
    let (
        external_ipc_rx,
        external_quit_rx,
        external_msg_tx,
        external_send_rx,
        ipc_listener_disabled,
        exit_on_quit,
    ) = match driver {
        DriverMode::Cli => (None, None, None, None, false, true),
        DriverMode::Mcp { ipc_rx, quit_rx } => {
            (Some(ipc_rx), Some(quit_rx), None, None, false, false)
        }
        DriverMode::Embed {
            msg_tx,
            send_rx,
            quit_rx,
        } => (
            None,
            Some(quit_rx),
            Some(msg_tx),
            Some(send_rx),
            true,
            false,
        ),
    };

    let started = Instant::now();
    let state_file = state_file.map(|path| StateFile::new(path, &swarm_str, &author));
    let state = EventLoopState::new(state_file, started);
    state.write_participant_count();

    // Origin co-hosts from t=0; a joiner defers to the `event_loop`
    // heal gate (`may_cohost`). Why: `EventLoopConfig::co_host_eagerly`.
    let mut rendezvous: Option<beacon::Rendezvous> = None;
    if co_host_eagerly {
        beacon::ensure(&rendezvous_params, &endpoint, &mut rendezvous).await;
    }

    let (sender, receiver) = topic.split();

    let ipc_rx = spawn_ipc_rx(
        external_ipc_rx,
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
    event_loop(EventLoop {
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
        co_host_eagerly,
        started,
        external_quit_rx,
        external_send_rx,
        external_msg_tx,
        quit_rx,
        exit_on_quit,
    })
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
    /// `true` ⇒ origin (`create`), co-host from t=0. `false` ⇒ joiner,
    /// defer co-hosting until meshed (or the empty-swarm grace).
    co_host_eagerly: bool,
    /// Event-loop start, for the unmeshed-joiner co-host grace.
    started: Instant,
    external_quit_rx: Option<mpsc::Receiver<()>>,
    external_send_rx: Option<mpsc::Receiver<SendRequest>>,
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
        rendezvous_params,
        co_host_eagerly,
        started,
        mut external_quit_rx,
        mut external_send_rx,
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
    let mut stdin_line = String::new();
    let mut stdin_open = interactive;

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
        tokio::select! {
            result = stdin_reader.read_line(&mut stdin_line), if stdin_open => {
                match result {
                    Ok(0) | Err(_) => { stdin_open = false; }
                    Ok(_) => {
                        gossip::handle_stdin_line(
                            stdin_line.trim(),
                            &sender,
                            &swarm_str,
                            &author,
                            &mut state,
                            &output,
                        ).await;
                        stdin_line.clear();
                    }
                }
            }
            ipc_msg = recv_opt(&mut ipc_rx) => {
                match ipc_msg {
                    None => { ipc_rx = None; }
                    Some((cmd, resp_tx)) => {
                        if ipc::handle_ipc_command(cmd, resp_tx, &swarm_str, &author, &mut state, &sender, &output).await {
                            state.last_sent_at = Instant::now();
                        }
                    }
                }
            }
            event = receiver.next(), if state.gossip_open => {
                gossip::handle_gossip_event(event, &mut state, &ctx).await;
            }
            _ = prune_interval.tick() => timers::tick_prune(&mut state),
            _ = alive_interval.tick() => lifecycle::heartbeat::tick_alive(&mut state, &sender, &swarm_str, &author).await,
            _ = sweep_interval.tick() => lifecycle::heartbeat::tick_sweep(&mut state, &output),
            _ = heal_interval.tick() => {
                gossip::heal::tick_heal(&endpoint, rendezvous_params.id, &sender).await;
                // Claim-if-free (private) / idempotent (public): take
                // over the beacon if the previous holder is gone — but
                // a joiner only once `may_cohost` (see its docs).
                if may_cohost(co_host_eagerly, state.meshed, started) {
                    beacon::ensure(&rendezvous_params, &endpoint, &mut rendezvous).await;
                }
            }
            _ = reclaim_interval.tick() => {
                maybe_reclaim(&state, &rendezvous_params, &endpoint, &mut rendezvous).await;
            }
            _ = antientropy_interval.tick() => {
                gossip::antientropy::broadcast_digest(&state, &sender, &swarm_str, &author).await;
            }
            _ = state_refresh_interval.tick() => timers::tick_state_refresh(&state),
            _ = recv_opt(&mut external_quit_rx) => {
                shutdown(&sender, &swarm_str, &swarm_name, &author, &state, &output).await;
                // External quit is always embedded (MCP): never exit
                // the process, regardless of `exit_on_quit`.
                break;
            }
            send_req = recv_opt(&mut external_send_rx) => {
                match send_req {
                    Some(req) => {
                        if gossip::handle_send_request(req, &swarm_str, &author, &mut state, &sender, &output).await {
                            state.last_sent_at = Instant::now();
                        }
                    }
                    None => external_send_rx = None,
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
    external_ipc_rx: Option<mpsc::Receiver<IpcMessage>>,
    disable_ipc_listener: bool,
    swarm: &SwarmId,
    author: &Nickname,
    output: output::Output,
) -> Option<mpsc::Receiver<IpcMessage>> {
    if let Some(rx) = external_ipc_rx {
        return Some(rx);
    }
    // Embed mode: no pre-wired channel AND no socket. Returning
    // `None` leaves the loop's IPC `select!` arm inert (it pends
    // forever), so the unix-socket listener is never bound.
    if disable_ipc_listener {
        return None;
    }
    let (ipc_tx, rx) = mpsc::channel::<IpcMessage>(32);
    tokio::spawn(listen(swarm.clone(), author.clone(), ipc_tx, output));
    Some(rx)
}

/// May this member co-host the rendezvous yet? See
/// [`EventLoopConfig::co_host_eagerly`] for the why. The origin always
/// may; a joiner only once `meshed`, or after `cohost_grace_secs` for
/// an empty swarm. Pure + cheap; never blocks `ready`.
fn may_cohost(co_host_eagerly: bool, meshed: bool, started: Instant) -> bool {
    co_host_eagerly
        || meshed
        || started.elapsed().as_secs() >= crate::util::tuning::cohost_grace_secs()
}

/// Fast event-driven failover: while the post-`NeighborDown` reclaim
/// window is open, retry the rendezvous claim so a survivor takes the
/// freed port in ~1s instead of waiting for the 15s heal tick. A no-op
/// outside the window (just an `Instant` compare) and idempotent once
/// the rendezvous is held.
async fn maybe_reclaim(
    state: &EventLoopState,
    params: &beacon::RendezvousParams,
    endpoint: &Endpoint,
    current: &mut Option<beacon::Rendezvous>,
) {
    if state
        .reclaim_until
        .is_some_and(|deadline| Instant::now() < deadline)
    {
        beacon::ensure(params, endpoint, current).await;
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
