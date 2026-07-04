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
use crate::util::consts::MAX_STDIN_LINE_BYTES;
use crate::{beacon, gossip, lifecycle, lookup};
// Bare `ipc` is `daemon::ipc`; transport's socket server is by-item.
use crate::transport::ipc::IpcMessage;
use crate::util::tuning::{
    ALIVE_INTERVAL_SECS, ANTIENTROPY_INTERVAL_SECS, HEAL_INTERVAL_SECS, RECLAIM_INTERVAL_MS,
    RESUBSCRIBE_MAX_ATTEMPTS, STATE_REFRESH_SECS, heal_stall_threshold_secs,
    ppid_watch_interval_ms, sweep_interval_secs,
};

use super::config::{CoHostPolicy, DriverMode, EventLoopConfig, SessionRequest};
use super::ctx::HandlerCtx;
use super::state::EventLoopState;
use super::{ipc, setup, timers};
use crate::a2a::task;

/// Never returns normally — exits the process on ctrl-c / SIGTERM.
pub(crate) async fn run(cfg: EventLoopConfig) -> Result<()> {
    let EventLoopConfig {
        topic,
        gossip,
        author,
        identity,
        swarm: swarm_str,
        name: swarm_name,
        output,
        interactive,
        endpoint,
        router: _router,
        max_peers,
        rendezvous_params,
        rung_rx,
        cohost,
        state_file,
        unicast_rx,
        a2a,
        live_count,
        driver,
    } = cfg;

    // Every driver-derived fact in one place. Only the CLI exits the
    // process on quit and binds the unix socket; in-process drivers
    // (embed / MCP) take typed requests on `req_rx` instead.
    let (
        external_quit_rx,
        external_msg_tx,
        external_req_rx,
        ipc_listener_disabled,
        exit_on_quit,
        handle_signals,
    ) = match driver {
        DriverMode::Cli => (None, None, None, false, true, true),
        DriverMode::InProcess {
            msg_tx,
            req_rx,
            quit_rx,
            handle_signals,
        } => (
            Some(quit_rx),
            msg_tx,
            Some(req_rx),
            true,
            false,
            handle_signals,
        ),
    };

    let started = Instant::now();
    // CLI `create`/`join` daemons default their state file into the swarm's
    // runtime folder (`<prefix>/<nick>.state.json`, beside the socket + log)
    // when no `--state-file` override is given. In-process embed/MCP sessions
    // (`!exit_on_quit`) keep writing nothing.
    let state_file = state_file
        .or_else(|| {
            exit_on_quit.then(|| {
                crate::util::swarm_runtime_dir(swarm_str.as_str())
                    .join(format!("{author}.state.json"))
            })
        })
        .map(|path| StateFile::new(path, &swarm_str, &author, &swarm_name));
    let (a2a_port, a2a_rx) = spawn_a2a(a2a, state_file.as_ref());
    let mut state = EventLoopState::new(state_file, started, identity);
    // Replace the detached default pool with one wired to this endpoint, so
    // directed sends can dial peers over the unicast ALPN.
    state.unicast_pool = crate::unicast::UnicastPool::new(endpoint.clone());
    // Advertise path only: the directory re-broadcast task reads the
    // live count from here. Set before the first write below so the
    // initial ad carries a real count.
    state.live_count = live_count;
    state.rendezvous_id = Some(rendezvous_params.id);
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

    let ipc_rx = spawn_ipc_rx(ipc_listener_disabled, &swarm_str, &author, &output);

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
        a2a_rx,
        a2a_port,
        unicast_rx: Some(unicast_rx),
    }))
    .await
}

/// The alive tick: note the gap, then broadcast the keepalive presence.
async fn alive_arm(
    anchors: &mut TickAnchors,
    state: &mut EventLoopState,
    sender: &GossipSender,
    swarm: &SwarmId,
    author: &Nickname,
) {
    timers::note_tick_gap(
        "alive",
        &mut anchors.alive,
        &mut anchors.alive_wall,
        Duration::from_secs(ALIVE_INTERVAL_SECS),
    );
    lifecycle::heartbeat::tick_alive(state, sender, swarm, author).await;
}

/// The anti-entropy tick: note the gap, advertise the chat digest, then both
/// channel digests, so peers can request anything we hold that they miss.
async fn antientropy_arm(
    anchors: &mut TickAnchors,
    state: &mut EventLoopState,
    sender: &GossipSender,
    swarm: &SwarmId,
    author: &Nickname,
) {
    timers::note_tick_gap(
        "antientropy",
        &mut anchors.antientropy,
        &mut anchors.antientropy_wall,
        Duration::from_secs(ANTIENTROPY_INTERVAL_SECS),
    );
    gossip::antientropy::broadcast_digest(state, sender, swarm, author).await;
    gossip::antientropy::broadcast_state_digests(state, sender, swarm, author).await;
}

/// One typed in-process session request (embed / MCP): dispatch it and
/// refresh the heartbeat clock when it broadcast. `false` means the channel
/// closed and polling should stop.
async fn handle_session_arm(
    req: Option<SessionRequest>,
    swarm: &SwarmId,
    author: &Nickname,
    state: &mut EventLoopState,
    sender: &GossipSender,
    output: &output::Output,
) -> bool {
    let Some(req) = req else {
        return false;
    };
    if gossip::handle_session_request(req, swarm, author, state, sender, output).await {
        state.last_sent_at = Instant::now();
    }
    true
}

/// The sweep-tick arm: note the gap, evict silent peers, then run the task
/// timers that ride the sweep cadence (each gates on its own elapsed-time
/// budget) — evict idle-debounce-expired tasks, then keepalive the ones
/// whose ball we still hold.
async fn sweep_arm(
    anchors: &mut TickAnchors,
    state: &mut EventLoopState,
    sender: &GossipSender,
    swarm: &SwarmId,
    author: &Nickname,
    output: &output::Output,
) {
    timers::note_tick_gap(
        "sweep",
        &mut anchors.sweep,
        &mut anchors.sweep_wall,
        Duration::from_secs(sweep_interval_secs()),
    );
    lifecycle::heartbeat::tick_sweep(state, output);
    task::tick_task_sweep(state, sender, swarm, author, output).await;
    task::tick_task_keepalive(state, sender, swarm, author).await;
}

/// One `--a2a-serve` JSON-RPC request from the HTTP task: execute against
/// the live loop state and answer on its oneshot. `false` means the channel
/// closed (the HTTP task is gone — daemon teardown) and polling should stop.
#[expect(
    clippy::too_many_arguments,
    reason = "the per-arm dispatch needs the same daemon coordinates as the IPC arm plus the binding's port"
)]
async fn handle_a2a_arm(
    req: Option<crate::a2a::rpc::A2aRequest>,
    swarm: &SwarmId,
    author: &Nickname,
    our_pubkey: &str,
    a2a_port: Option<u16>,
    state: &mut EventLoopState,
    sender: &GossipSender,
    output: &output::Output,
) -> bool {
    let Some(crate::a2a::rpc::A2aRequest { op, resp }) = req else {
        return false;
    };
    // A directed `message/send` (task creation) needs the synchronous gossip
    // request/response round-trip — the peer mints the task id and returns the
    // Task. Route it through the waiter so the HTTP handler's oneshot resolves
    // when the peer answers (or times out); the event loop is never blocked.
    // This is what lets an off-the-shelf A2A client delegate a task over the
    // compliant localhost binding. Everything else executes inline.
    if let crate::a2a::rpc::A2aOp::SendMessage {
        to: Some(peer),
        message,
    } = op
    {
        gossip::broadcast_a2a_call(
            swarm,
            author,
            peer,
            "SendMessage",
            serde_json::json!({ "message": message }),
            Duration::from_secs(30),
            crate::daemon::state::A2aResponder::Rpc(resp),
            state,
            sender,
        )
        .await;
        return true;
    }
    let outcome = crate::a2a::rpc::handle_op(
        op,
        swarm,
        author,
        our_pubkey,
        a2a_port.unwrap_or_default(),
        state,
        sender,
        output,
    )
    .await;
    let _ = resp.send(outcome);
    true
}

/// `--a2a-serve`: note the bound port + bearer token in the state file (the
/// local client's discovery channel; the file is chmod 600 because of the
/// token), then hand the listener to the HTTP task. Requests come back
/// through the returned receiver into the select loop. `(None, None)` when
/// the binding is off.
fn spawn_a2a(
    a2a: Option<crate::a2a::http::A2aBinding>,
    state_file: Option<&StateFile>,
) -> (
    Option<u16>,
    Option<mpsc::Receiver<crate::a2a::rpc::A2aRequest>>,
) {
    let Some(binding) = a2a else {
        return (None, None);
    };
    let port = binding.port;
    if let Some(sf) = state_file {
        sf.set_a2a(port, binding.token.clone());
    }
    let (a2a_tx, a2a_rx) =
        mpsc::channel::<crate::a2a::rpc::A2aRequest>(crate::a2a::http::REQUEST_QUEUE);
    crate::a2a::http::spawn(binding, a2a_tx);
    (Some(port), Some(a2a_rx))
}

/// Owned working set for [`event_loop`]. `run` does setup, fills this,
/// and hands it over; the loop destructures it back into the same
/// locals the orchestrator used to hold inline. Splitting the 11-arm
/// `select!` out keeps both functions within the readability budget
/// (clippy `too_many_lines`) without an `#[allow]`.
struct EventLoop {
    sender: GossipSender,
    receiver: GossipReceiver,
    /// The gossip frontend, kept so the loop can re-subscribe the topic
    /// after the stream terminally ends (see the heal arm) — without it
    /// a closed subscription (e.g. lag-evicted by the actor) would
    /// leave the daemon permanently deaf.
    gossip: iroh_gossip::net::Gossip,
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
    /// The `--a2a-serve` request channel + bound port (`None` = binding off).
    a2a_rx: Option<mpsc::Receiver<crate::a2a::rpc::A2aRequest>>,
    a2a_port: Option<u16>,
    /// Inbound unicast frames from the `UNICAST_ALPN` acceptor, drained into
    /// `gossip::ingest` (same validation + dedup path as gossip). `Option` so
    /// the `select!` arm can disable itself if the channel ever closes.
    unicast_rx: Option<mpsc::Receiver<bytes::Bytes>>,
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
        target: "agent_gossip::lifecycle",
        version = crate::VERSION,
        nickname = %author,
        "daemon starting"
    );
}

/// Publish this member's `AgentCard` at meta `/peers/<nick>/card` — the one
/// channel write the daemon itself makes (documented glossary exception: the
/// card is the peer's canonical A2A self-description, architectural rather
/// than app state). Unmeshed it buffers/backfills like any state event;
/// agent-side facts (model/harness/host) stay the agent's merge.
pub(crate) async fn publish_own_card(
    swarm: &SwarmId,
    author: &Nickname,
    our_pubkey: &str,
    state: &mut EventLoopState,
    sender: &GossipSender,
    output: &output::Output,
    endpoint: &Endpoint,
) {
    let seal_b58 = bs58::encode(state.identity.seal_public()).into_string();
    let card = crate::a2a::card::own_card(author, our_pubkey, &seal_b58);
    // Fold our stable dial hint (EndpointId + home relay) into the card so a peer
    // that has synced the meta doc can dial us without a gossiped `PeerInfo`.
    // Re-publishing with an unchanged address is a no-op (an automerge change
    // with no ops), so this is safe to call repeatedly (e.g. on mesh/relay
    // changes) and only writes when the relay actually moves.
    let mut merge = crate::a2a::card::publish_merge(author, &card);
    merge["peers"][author.as_str()]["card"]["endpoint"] = crate::a2a::card::endpoint_hint(endpoint);
    if let Err(error) = gossip::broadcast_state_merge(
        swarm,
        author,
        merge,
        state,
        sender,
        output,
        crate::protocol::Channel::Meta,
        // Internal plumbing: the agent didn't write this, so don't surface it as
        // a "you changed shared state" event (nor race a fetch long-poll).
        false,
    )
    .await
    {
        tracing::warn!(%error, "failed to publish this member's agent card");
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the daemon's central select! loop: one arm per event source (stdin, ipc, a2a, gossip, the maintenance ticks, quit); each arm delegates to a helper, but the arm list itself is irreducibly long"
)]
async fn event_loop(loop_state: EventLoop) -> Result<()> {
    let EventLoop {
        mut sender,
        mut receiver,
        gossip,
        endpoint,
        swarm: swarm_str,
        name: swarm_name,
        author,
        output,
        max_peers,
        mut state,
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
        a2a_port,
        mut unicast_rx,
    } = loop_state;

    log_daemon_start(&author);

    // Mirror every surfaced event into `state.surfaced_events` (the
    // `poll`/`fetch` history) via an `Output` tap. Kept entirely inside the
    // loop: `EventLoopConfig` carries no receiver. `with_tap` attaches to the
    // CLI `Stream` sink AND the embed/MCP `Capture` sink (so embed `fetch` /
    // MCP `fetch_messages` see the same events) — the embed path relies on
    // that. The channel is unbounded but drained every iteration, so it holds
    // at most one iteration's worth of events.
    //
    // Note this taps the event loop's OWN `output` only; the IPC socket
    // listener was handed a separate `output.clone()` in `run()` BEFORE this
    // tap, so its clone is untapped. That is fine: the listener uses its clone
    // only for socket bind/accept errors, and IPC-command events are surfaced
    // by the loop's tapped `output` here (the listener just forwards parsed
    // commands over a channel).
    let (surfaced_tx, mut surfaced_rx) = mpsc::unbounded_channel::<output::OutputEvent>();
    let output = output.with_tap(surfaced_tx);

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
        swarm: &swarm_str,
        author: &author,
        identity: identity.as_ref(),
        our_pubkey: &our_pubkey,
        max_peers,
        rendezvous_id: rendezvous_params.id,
        external_msg_tx: external_msg_tx.as_ref(),
        output: &output,
    };

    // Consecutive failed resubscribe attempts (reset on success); at
    // `RESUBSCRIBE_MAX_ATTEMPTS` the gossip actor itself is gone and the
    // daemon shuts down rather than pretend to be a live member.
    let mut resubscribe_attempts: u32 = 0;

    publish_own_card(
        &swarm_str,
        &author,
        &our_pubkey,
        &mut state,
        &sender,
        &output,
        &endpoint,
    )
    .await;

    loop {
        tokio::select! {
            result = read_bounded_line(&mut stdin_reader, MAX_STDIN_LINE_BYTES), if stdin_open =>
                stdin_open = handle_stdin_arm(result, &sender, &swarm_str, &author, &mut state, &output).await,
            () = sleep_until_opt(state.ping_round.as_ref().map(|round| round.deadline)) =>
                finalize_ping_round(&mut state, &output),
            () = sleep_until_opt(state.earliest_poll_deadline()) => poll_deadline_arm(&mut state),
            () = sleep_until_opt(state.earliest_a2a_deadline()) =>
                state.expire_a2a_waiters(tokio::time::Instant::now()),
            ipc_msg = recv_opt(&mut ipc_rx) => {
                if !handle_ipc_arm(ipc_msg, &swarm_str, &swarm_name, &author, &mut state, &sender, &output).await {
                    ipc_rx = None;
                }
            }
            a2a_req = recv_opt(&mut a2a_rx) => {
                if !handle_a2a_arm(a2a_req, &swarm_str, &author, &our_pubkey, a2a_port, &mut state, &sender, &output).await {
                    a2a_rx = None;
                }
            }
            event = receiver.next(), if state.gossip_open => {
                let ctx = parts.ctx(&sender);
                gossip::handle_gossip_event(event, &mut state, &ctx).await;
            }
            // Inbound unicast rides the *same* validate + dedup path as gossip (`ingest`).
            frame = recv_opt(&mut unicast_rx) => match frame {
                Some(bytes) => gossip::ingest(bytes, &mut state, &parts.ctx(&sender)).await,
                None => unicast_rx = None,
            },
            _ = intervals.prune.tick() => timers::tick_prune(&mut state, &output),
            _ = intervals.alive.tick() => {
                alive_arm(&mut anchors, &mut state, &sender, &swarm_str, &author).await;
            }
            _ = intervals.sweep.tick() => {
                sweep_arm(&mut anchors, &mut state, &sender, &swarm_str, &author, &output).await;
            }
            _ = intervals.heal.tick() => {
                let (mono_gap, wall_gap) = timers::note_tick_gap("heal", &mut anchors.heal, &mut anchors.heal_wall, Duration::from_secs(HEAL_INTERVAL_SECS));
                if state.gossip_open {
                    let ctx = parts.ctx(&sender);
                    heal_tick(mono_gap, wall_gap, &mut state, &ctx, &rendezvous_params, cohost, started, &mut rendezvous).await;
                } else {
                    // Stream ended: resubscribe instead of healing a dead topic
                    // (see `resubscribe_tick`); the beacon keeps the swarm joinable.
                    resubscribe_tick(&gossip, &rendezvous_params, &parts, &mut state, &mut sender, &mut receiver, &mut resubscribe_attempts, exit_on_quit).await?;
                    maybe_cohost(cohost, &state, started, &rendezvous_params, &endpoint, &mut rendezvous).await;
                }
            }
            // A bootstrap rung chosen off-loop (startup probe / beacon self-monitor); apply it cheaply.
            // `Ok(())` only: a closed channel (impossible while the beacon params live) disables the arm.
            Ok(()) = rung_rx.changed() => apply_rung_change(&mut rendezvous_params, &endpoint, &mut rendezvous, &rung_rx),
            _ = intervals.reclaim.tick() =>
                maybe_reclaim(cohost, &state, &rendezvous_params, &endpoint, &mut rendezvous).await,
            _ = intervals.antientropy.tick() => {
                antientropy_arm(&mut anchors, &mut state, &sender, &swarm_str, &author).await;
            }
            _ = intervals.state_refresh.tick() => timers::tick_state_refresh(&state, &endpoint).await,
            _ = recv_opt(&mut external_quit_rx) => {
                // External quit is always embedded (MCP): never hard-exit (`false`).
                announce_and_maybe_exit(&sender, &swarm_str, &swarm_name, &author, &mut state, &output, false).await;
                break;
            }
            req = recv_opt(&mut external_req_rx) => {
                if !handle_session_arm(req, &swarm_str, &author, &mut state, &sender, &output).await {
                    external_req_rx = None;
                }
            }
            _ = quit_rx.recv() => {
                announce_and_maybe_exit(&sender, &swarm_str, &swarm_name, &author, &mut state, &output, exit_on_quit).await;
                break;
            }
        }
        drain_surfaced(&mut surfaced_rx, &mut state);
    }

    Ok(())
}

/// Drain the `Output` tap into the surfaced-events ring, then fulfill any
/// long-poll waiter the newly-drained events advanced past (fulfill must follow
/// the drain). Runs after the arm that produced the events, before the next
/// iteration can serve a `poll`, so a poll never misses an event surfaced in a
/// prior iteration. `try_recv` is non-blocking; the channel is empty in the
/// steady state.
///
/// Keeps only events that belong in the `poll`/`fetch` history; drops the rest.
fn drain_surfaced(
    surfaced_rx: &mut mpsc::UnboundedReceiver<output::OutputEvent>,
    state: &mut EventLoopState,
) {
    while let Ok(event) = surfaced_rx.try_recv() {
        if is_pollable(&event) {
            state.surfaced_events.push(event);
        }
    }
    state.fulfill_ready_poll_waiters();
}

/// Whether a surfaced event belongs in the `poll`/`fetch` history — an explicit
/// allow-list of the documented pollable contract (chat, presence joined/left,
/// content task legs, and the transient `ping_report`/`peer_timeout`/
/// `peer_return`/`task_timeout`/`fork`). Deliberately an allow-list, not
/// "everything except X": operational notices (`info`/`error`/`msg_posted`) and
/// startup events (`ready`/`swarm_id`) also flow through the same `Output` tap,
/// and must NOT enter the ring — they are developer/stream plumbing the poll
/// contract never promised, and `poll_since`'s own eviction notices would
/// otherwise feed back into the ring being polled. The `task` `Progress`
/// beat is excluded too (a liveness widget update, never a retained record).
fn is_pollable(event: &output::OutputEvent) -> bool {
    use output::OutputEvent;
    match event {
        OutputEvent::Message { .. }
        | OutputEvent::Presence { .. }
        | OutputEvent::PingReport { .. }
        | OutputEvent::PeerTimeout { .. }
        | OutputEvent::PeerReturn { .. }
        | OutputEvent::TaskTimeout { .. }
        | OutputEvent::TaskMessage { .. }
        | OutputEvent::StateChanged { .. }
        | OutputEvent::Fork { .. } => true,
        OutputEvent::Task { msg, .. } => !crate::a2a::gossip::status_payload(msg)
            .is_ok_and(|payload| crate::a2a::gossip::is_beat(&payload)),
        OutputEvent::Info { .. }
        | OutputEvent::Error { .. }
        | OutputEvent::MsgPosted { .. }
        | OutputEvent::Ready { .. }
        | OutputEvent::SwarmId { .. } => false,
    }
}

/// Graceful shutdown: remove the statusline state file first, then
/// announce `Left` and give the broadcast a moment to reach peers.
/// Shared by both the external-quit and ctrl-c/SIGTERM/SIGHUP paths so
/// they can't drift apart.
///
/// The state-file removal is the time-critical step: an external reader
/// (the shell statusline) shows the swarm pill while the file is fresh,
/// so a leaver must clear it *immediately*. It runs before the
/// best-effort `Left` broadcast and its 500 ms propagation sleep so a
/// kill landing during that window can't strand the file with a still-fresh
/// `last_updated`, leaving a ghost pill on the statusline.
async fn shutdown(
    sender: &GossipSender,
    swarm: &SwarmId,
    name: &SwarmName,
    author: &Nickname,
    state: &mut EventLoopState,
    output: &output::Output,
) {
    // Close the blob-serving endpoint (if we ever bound one); dropping its store
    // removes the `<nick>.blobs/` spool.
    if let Some(server) = state.blob_server.take() {
        server.shutdown().await;
    }
    if let Some(sf) = state.state_file.as_ref() {
        sf.remove();
    }
    output.info(&format!("left #{name}"));
    lifecycle::log_leaving(name.as_str());
    gossip::broadcast_msg(
        sender,
        &Message::new_left(swarm, author).signed(&state.identity),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
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
async fn announce_and_maybe_exit(
    sender: &GossipSender,
    swarm: &SwarmId,
    name: &SwarmName,
    author: &Nickname,
    state: &mut EventLoopState,
    output: &output::Output,
    exit_on_quit: bool,
) {
    // Empty out any parked long-poll waiters first, so a held call returns a
    // clean timeout (empty) rather than a dropped-channel error — and before
    // the `exit_on_quit` path below may `std::process::exit`.
    state.close_poll_waiters();
    state.close_a2a_waiters();
    shutdown(sender, swarm, name, author, state, output).await;
    #[cfg(not(feature = "dhat-heap"))]
    if exit_on_quit {
        std::process::exit(0);
    }
    #[cfg(feature = "dhat-heap")]
    let _ = exit_on_quit;
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
/// daemon for a `/gossip:*` session) tends to send; without catching it
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
/// of terminated and would otherwise linger in the swarm forever. The daemon
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
fn spawn_ipc_rx(
    disable_ipc_listener: bool,
    swarm: &SwarmId,
    author: &Nickname,
    output: &output::Output,
) -> Option<mpsc::Receiver<IpcMessage>> {
    // In-process mode (embed / MCP): no socket. Returning `None` leaves
    // the loop's IPC `select!` arm inert (it pends forever), so the
    // unix-socket listener is never bound — those drivers use the typed
    // `req_rx` instead.
    if disable_ipc_listener {
        return None;
    }
    // Bind synchronously here, *before* the caller marks the session ready,
    // so an accepting socket always exists by the time the readiness flag
    // flips. Only the (always-running) accept loop is spawned. A bind
    // failure is non-fatal — the daemon still gossips; it just can't take
    // IPC — matching the prior best-effort behavior, only now observed
    // before readiness rather than racing it.
    let listener = match crate::transport::ipc::bind(swarm, author) {
        Ok(listener) => listener,
        Err(error) => {
            output.error(&format!("IPC: {error}"));
            tracing::warn!(%error, "IPC: failed to bind socket");
            return None;
        }
    };
    let (ipc_tx, rx) = mpsc::channel::<IpcMessage>(32);
    tokio::spawn(crate::transport::ipc::serve(
        listener,
        ipc_tx,
        output.clone(),
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

/// The earliest long-poll deadline elapsed: fulfill any waiter a same-instant
/// event made ready (so it wins over the timeout), then expire the rest.
fn poll_deadline_arm(state: &mut EventLoopState) {
    state.fulfill_ready_poll_waiters();
    state.expire_poll_waiters(tokio::time::Instant::now());
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
    output.ping_report(peers, known);
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
    ctx: &HandlerCtx<'_>,
    params: &beacon::RendezvousParams,
) {
    let threshold = Duration::from_secs(heal_stall_threshold_secs());
    let hard_edge = is_resume(mono_gap, threshold) || is_wall_resume(wall_gap, mono_gap, threshold);
    if hard_edge {
        tracing::warn!(
            target: "agent_gossip::gossip",
            mono_gap_ms = u64::try_from(mono_gap.as_millis()).unwrap_or(u64::MAX),
            wall_gap_ms = u64::try_from(wall_gap.as_millis()).unwrap_or(u64::MAX),
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
            target: "agent_gossip::gossip",
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
    ctx: &HandlerCtx<'_>,
    params: &beacon::RendezvousParams,
    cohost: CoHostPolicy,
    started: Instant,
    rendezvous: &mut Option<beacon::Rendezvous>,
) {
    run_heal(mono_gap, wall_gap, state, ctx, params).await;
    maybe_cohost(cohost, state, started, params, ctx.endpoint, rendezvous).await;
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
    swarm: &'a SwarmId,
    author: &'a Nickname,
    identity: &'a crate::protocol::identity::Identity,
    our_pubkey: &'a str,
    max_peers: usize,
    rendezvous_id: iroh::EndpointId,
    external_msg_tx: Option<&'a broadcast::Sender<Message>>,
    output: &'a output::Output,
}

impl<'a> CtxParts<'a> {
    fn ctx<'b>(&'b self, sender: &'b GossipSender) -> HandlerCtx<'b>
    where
        'a: 'b,
    {
        HandlerCtx {
            sender,
            endpoint: self.endpoint,
            swarm: self.swarm,
            author: self.author,
            identity: self.identity,
            our_pubkey: self.our_pubkey,
            max_peers: self.max_peers,
            rendezvous_id: self.rendezvous_id,
            external_msg_tx: self.external_msg_tx,
            output: self.output,
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
#[expect(
    clippy::too_many_arguments,
    reason = "threads the loop-owned swap targets (sender/receiver) plus the ctx parts; bundling them would just re-wrap the event loop's locals"
)]
async fn resubscribe_tick(
    gossip: &iroh_gossip::net::Gossip,
    params: &beacon::RendezvousParams,
    parts: &CtxParts<'_>,
    state: &mut EventLoopState,
    sender: &mut GossipSender,
    receiver: &mut GossipReceiver,
    attempts: &mut u32,
    exit_on_quit: bool,
) -> Result<()> {
    match try_resubscribe(gossip, params, state, attempts, parts.output).await {
        Resubscribe::Restored(new_sender, new_receiver) => {
            let mut dead_receiver = std::mem::replace(receiver, new_receiver);
            *sender = new_sender;
            state.gossip_open = true;
            // The dead subscription's link view is void; the fresh one
            // emits its own NeighborUps (and re-arms the probe gate).
            state.rendezvous_linked = false;
            let ctx = parts.ctx(sender);
            gossip::drain_dead_receiver(&mut dead_receiver, state, &ctx).await;
            drop(dead_receiver);
            gossip::heal::recover_from_starvation(state, &ctx).await;
        }
        Resubscribe::Pending => {}
        Resubscribe::Fatal => {
            if let Some(state_file) = state.state_file.as_ref() {
                state_file.remove();
            }
            parts
                .output
                .error("gossip subscription unrecoverable; shutting down");
            #[cfg(not(feature = "dhat-heap"))]
            if exit_on_quit {
                std::process::exit(1);
            }
            #[cfg(feature = "dhat-heap")]
            let _ = exit_on_quit;
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
    gossip: &iroh_gossip::net::Gossip,
    params: &beacon::RendezvousParams,
    state: &EventLoopState,
    attempts: &mut u32,
    output: &output::Output,
) -> Resubscribe {
    let mut bootstrap = vec![params.id];
    bootstrap.extend(state.known_endpoints.iter().copied());
    match gossip.subscribe(params.topic_id, bootstrap).await {
        Ok(topic) => {
            *attempts = 0;
            tracing::warn!(
                target: "agent_gossip::gossip",
                "gossip stream restored (resubscribed)"
            );
            output.info("gossip stream restored; rejoining the mesh");
            let (sender, receiver) = topic.split();
            Resubscribe::Restored(sender, receiver)
        }
        Err(error) => {
            *attempts += 1;
            tracing::warn!(
                target: "agent_gossip::gossip",
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
            target: "agent_gossip::beacon",
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
    name: &SwarmName,
    author: &Nickname,
    state: &mut EventLoopState,
    sender: &GossipSender,
    output: &output::Output,
) -> bool {
    match ipc_msg {
        None => false,
        Some((cmd, resp_tx)) => {
            if ipc::handle_ipc_command(cmd, resp_tx, swarm, name, author, state, sender, output)
                .await
            {
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
    let mut heal = tokio::time::interval(Duration::from_secs(HEAL_INTERVAL_SECS));
    heal.set_missed_tick_behavior(Skip);
    heal.tick().await;
    let mut reclaim = tokio::time::interval(Duration::from_millis(RECLAIM_INTERVAL_MS));
    reclaim.set_missed_tick_behavior(Skip);
    reclaim.tick().await;
    let mut antientropy = tokio::time::interval(Duration::from_secs(ANTIENTROPY_INTERVAL_SECS));
    antientropy.set_missed_tick_behavior(Skip);
    antientropy.tick().await;
    let mut state_refresh = tokio::time::interval(Duration::from_secs(STATE_REFRESH_SECS));
    state_refresh.set_missed_tick_behavior(Skip);
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
