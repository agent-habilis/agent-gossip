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
use tokio::io::BufReader;
use tokio::sync::{broadcast, mpsc};

use crate::output;
use crate::protocol::swarm::SwarmName;
use crate::protocol::{Message, Nickname, SwarmId};
use crate::util::bounded_read::{LineRead, read_bounded_line};
use crate::util::state_file::StateFile;
use crate::{beacon, gossip, lifecycle};
use ahs_shared::MAX_STDIN_LINE_BYTES;
// Bare `ipc` is `daemon::ipc`; transport's socket server is by-item.
use crate::transport::ipc::{IpcMessage, listen};
use crate::util::tuning::{
    ALIVE_INTERVAL_SECS, ANTIENTROPY_INTERVAL_SECS, HEAL_INTERVAL_SECS, RECLAIM_INTERVAL_MS,
    STATE_REFRESH_SECS, heal_stall_threshold_secs, sweep_interval_secs,
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
    let state_file = state_file.map(|path| StateFile::new(path, &swarm_str, &author, &swarm_name));
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
        co_host_eagerly,
        started,
        external_quit_rx,
        external_send_rx,
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
                run_heal(mono_gap, wall_gap, &mut state, &endpoint, &sender, &rendezvous_params).await;
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
                timers::note_tick_gap("antientropy", &mut last_antientropy, &mut last_antientropy_wall, Duration::from_secs(ANTIENTROPY_INTERVAL_SECS));
                gossip::antientropy::broadcast_digest(&state, &sender, &swarm_str, &author).await;
            }
            _ = state_refresh_interval.tick() => timers::tick_state_refresh(&state, &endpoint).await,
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
            nickname: nickname.as_str().to_owned(),
            rtt_ms: u64::try_from(arrival.duration_since(round.t1).as_millis()).unwrap_or(u64::MAX),
        })
        .collect();
    peers.sort_by(|left, right| left.nickname.cmp(&right.nickname));
    output.ping_report(peers, state.participants.len());
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{is_resume, is_wall_resume};

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
