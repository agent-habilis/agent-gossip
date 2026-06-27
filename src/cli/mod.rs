//! The `ahsw` command-line interface: the clap-derived argument shape
//! lives in [`args`], the live `discover` picker in [`discover`], and the
//! per-subcommand handlers + [`dispatch`] here. `lib.rs::run_cli` parses
//! argv and calls `dispatch`; each handler is the thin glue between the
//! parsed args and the daemon / IPC / embed layers it drives.

use anyhow::Result;
use serde::Deserialize;

use crate::daemon::run as run_event_loop;
use crate::daemon::setup::{SetupKind, setup_swarm};
use crate::daemon::{CreateParams, JoinParams, Resolved};
use crate::embed::spawn_advertiser;
use crate::output::{Output, OutputMode};
use crate::protocol::swarm::{SwarmConfig, SwarmName, resolve_lookups};
use crate::protocol::{MessageId, Nickname};
use crate::resolver::JoinTarget;
use crate::transport::ipc::{self, IpcCommand};

pub(crate) mod agent;
mod args;
mod discover;
mod plug;
mod status;

pub(crate) use args::Cli;
use args::{
    Commands, CreateOpts, ExchangeOpts, MsgOpts, PeersOpts, PingOpts, PollOpts, ReadyOpts,
    SharedServerOpts, StateAction, StateOpts,
};

/// `join` has no `--public`/`--name`: both are encoded in the `ahsw…`
/// identifier and auto-detected. Without this, clap rejects them with
/// a generic "unexpected argument" + a misleading "pass as a value"
/// tip; this gives the real reason instead.
fn reject_id_encoded_flag(flag: &str, present: bool) -> Result<()> {
    if present {
        anyhow::bail!(
            "`{flag}` is not valid for `join`: the swarm's network mode \
             and name are encoded in the swarm id and auto-detected. \
             Drop `{flag}` — `join` takes only the id and `--nickname`."
        );
    }
    Ok(())
}

/// Run the selected subcommand to completion.
///
/// # Errors
/// Propagates any error from the selected subcommand — swarm setup
/// failure, join timeout, IPC errors, invalid swarm-mode flags, etc.
pub(crate) async fn dispatch(cli: Cli) -> Result<()> {
    // Install the log-path config from the global flags before any
    // subcommand resolves its log file (the buffered sink flushes at
    // `logging::attach`, after this). Replaces the old AHS_LOG_DIR /
    // AHS_LOG_MAX_BYTES env reads.
    crate::util::logs::configure(crate::util::logs::LogConfig {
        dir: cli.log_dir,
        max_bytes: cli.log_max_bytes,
        raw: cli.log_raw,
    });
    // Source the process tuning from the (hidden) flags before any handler
    // resolution reads it — e.g. `create --advertise` checks the
    // `directory_private` guard while building its `SetupKind`, before the
    // event loop. Replaces the former env-var overrides.
    match cli.command {
        // The event-loop futures (`create`/`join`/`discover`/`mcp::run`) are
        // boxed: each holds a full `EventLoopState`/session, so they sit right at
        // clippy's `large_futures` 16 KiB threshold — boxing keeps the dispatch
        // future small and the size off the knife's edge as those types grow.
        Commands::Create { opts } => {
            crate::util::tuning::init(opts.shared.tuning());
            Box::pin(create(opts)).await
        }
        Commands::Join { opts } => {
            reject_id_encoded_flag("--public", opts.public)?;
            reject_id_encoded_flag("--name", opts.name.is_some())?;
            crate::util::tuning::init(opts.shared.tuning());
            Box::pin(join(opts.swarm, opts.nickname, opts.shared)).await
        }
        Commands::Msg { opts } => msg(opts).await,
        Commands::Poll { opts } => poll(opts).await,
        Commands::Ping { opts } => ping(opts).await,
        Commands::Exchange { opts } => exchange(opts).await,
        Commands::Peers { opts } => peers(opts).await,
        Commands::State { opts } => state(opts).await,
        Commands::Ready { opts } => ready(opts).await,
        Commands::Discover { opts } => {
            crate::util::tuning::init(opts.shared.tuning());
            Box::pin(discover::discover(opts)).await
        }
        Commands::Mcp {
            directory_private,
            ping_window_secs,
        } => {
            // The MCP server holds no `SharedServerOpts`; install just the two
            // hidden knobs the suite varies (loopback directory, short ping
            // window) over the production defaults.
            crate::util::tuning::init(crate::util::tuning::Tuning {
                ping_window_secs,
                directory_private,
                ..crate::util::tuning::Tuning::DEFAULTS
            });
            Box::pin(crate::mcp::run()).await
        }
        // The manual is embedded at compile time (`include_str!`), so the
        // binary documents itself with no repo checkout.
        Commands::Man => {
            print!("{}", include_str!("../../docs/manual.txt"));
            Ok(())
        }
        // Embedded integration artifacts written to the agents' skills dirs —
        // self-contained, no repo checkout needed (like `Man`).
        Commands::Plug { agents } => plug::plug(&agents),
        Commands::Unplug { agents } => plug::unplug(&agents),
        Commands::Status => status::run(),
    }
}

/// Build the output sink, set up the swarm, and run the event loop. The
/// shared spine of `create` and `join` — `resolved` carries the
/// already-resolved [`SetupKind`](crate::daemon::setup::SetupKind), author,
/// and advertise directory (see [`crate::daemon::params`]).
async fn run_session(resolved: Resolved, shared: SharedServerOpts) -> Result<()> {
    let Resolved {
        kind,
        author,
        advertise_directory,
    } = resolved;
    // The advertiser reaches the directory over this swarm's own lookups
    // (only `create` advertises, so only `Create` carries them).
    let directory_lookups = match &kind {
        SetupKind::Create { config, .. } => Some(config.lookups.clone()),
        SetupKind::Join { .. } => None,
    };
    let out = Output::new(
        shared.output.into(),
        shared.filter_self,
        Some(author.as_str().to_owned()),
    );
    // Nag once at startup if an installed integration has fallen behind this
    // binary. CLI-only: the embed/MCP paths pass `None` so in-process tests
    // stay hermetic. `ahsw status` is the on-demand counterpart.
    let drift = agent::home_dir()
        .ok()
        .and_then(|home| agent::drift_warning(&home));
    let mut cfg = setup_swarm(
        kind,
        author,
        !shared.no_interactive,
        shared.max_peers,
        shared.state_file,
        out,
        drift.as_deref(),
    )
    .await?;
    // Self-reported model / harness, announced in our `joined` body. Set here
    // (not in `setup_swarm`) to keep its arg count in budget — same
    // late-assignment pattern as `live_count`.
    cfg.model = shared.model;
    cfg.harness = shared.harness;
    // Advertising (`create --advertise`): start the re-broadcast task. It
    // reaches the directory over this swarm's own lookups. The handle is
    // held for the session's lifetime — on the CLI the process exits (via
    // signal) before it would drop, which tears the task down.
    let _advertiser = advertise_directory
        .zip(directory_lookups)
        .map(|(directory, lookups)| spawn_advertiser(&mut cfg, directory, lookups));
    // First point where swarm id + nickname are known — attach the
    // buffered log sink here (see `logging`).
    crate::logging::attach(&cfg.swarm, &cfg.author);
    run_event_loop(cfg).await
}

/// Create a new swarm, print its identifier, and start listening.
async fn create(opts: CreateOpts) -> Result<()> {
    // Borrow-then-move: resolve the lookups and advertise selection (which
    // borrow `opts`) before moving `opts.name`/`opts.nickname` out.
    let advertise = opts.advertise_selection();
    let config = SwarmConfig {
        rate_limit_per_min: opts.rate_limit,
        lookups: resolve_lookups(opts.public, opts.lookups.to_set()),
    };
    // `resolve` validates `--advertise` against the config (never a silent
    // no-op) before any setup work.
    let resolved = CreateParams {
        name: opts.name.unwrap_or_else(SwarmName::random),
        nickname: opts.nickname,
        config,
        advertise,
    }
    .resolve()?;
    run_session(resolved, opts.shared).await
}

/// Join an existing swarm by its identifier (ahsw...), a domain, or a
/// supported git repo URL. The swarm's config (lookups + rate limit) is
/// decoded from the id — `join` takes no lookup/rate flags.
async fn join(
    target: JoinTarget,
    nickname: Option<Nickname>,
    shared: SharedServerOpts,
) -> Result<()> {
    // `join` never advertises — that is a create-time decision.
    let resolved = JoinParams { target, nickname }.resolve().await?;
    run_session(resolved, shared).await
}

#[derive(Deserialize)]
struct MsgResponse {
    ok: bool,
    id: Option<MessageId>,
    error: Option<String>,
    #[serde(default)]
    rate_limited: bool,
}

/// Reduce an IPC send response (`msg` / `handover`, same
/// `{ok,id,error,rate_limited}` shape) to the new message id, or a
/// descriptive error. `what` names the operation for the rate-limit and
/// missing-id messages. A rate-limited send is a deliberate drop, not a
/// failure — surfaced as a (still non-zero) error so scripts see it wasn't
/// sent.
fn finish_send(resp: &str, what: &str) -> Result<MessageId> {
    let parsed: MsgResponse = serde_json::from_str(resp)?;
    if parsed.rate_limited {
        anyhow::bail!("rate limit exceeded — {what} not sent");
    }
    if !parsed.ok {
        anyhow::bail!(
            "{}",
            parsed.error.unwrap_or_else(|| "unknown error".to_string())
        );
    }
    parsed
        .id
        .ok_or_else(|| anyhow::anyhow!("{what} response missing id"))
}

/// Post a message to a swarm via the running server's IPC socket.
async fn msg(opts: MsgOpts) -> Result<()> {
    let MsgOpts {
        swarm,
        nickname,
        text,
        reply,
    } = opts;
    let cmd = IpcCommand::Msg {
        swarm,
        body: text,
        reply,
    };

    let resp = ipc::send(&cmd, &nickname).await?;
    let id = finish_send(&resp, "message")?;
    // `msg` has no `--output` flag — always the human confirmation.
    // No nickname is rendered here (only `message posted` + id).
    let out = Output::new(OutputMode::Human, false, None);
    out.msg_posted(&id);

    Ok(())
}

/// Retrieve buffered messages from a running swarm process via IPC.
/// `poll` always emits the raw IPC JSON; the `--output` flag is accepted
/// for symmetry but not consulted here.
async fn poll(opts: PollOpts) -> Result<()> {
    let PollOpts {
        swarm,
        nickname,
        after,
        wait,
        output: _,
    } = opts;
    let cmd = IpcCommand::Poll {
        swarm,
        after,
        wait_ms: wait,
    };

    let resp = ipc::send(&cmd, &nickname).await?;
    println!("{resp}");

    Ok(())
}

/// Arm an RTT round on the running daemon. Fire-and-forget: the daemon
/// acks immediately and emits the `ping_report` on its own
/// `--output json` stream once the collection window closes.
async fn ping(opts: PingOpts) -> Result<()> {
    let PingOpts { swarm, nickname } = opts;
    let cmd = IpcCommand::Ping { swarm };
    let resp = ipc::send(&cmd, &nickname).await?;
    let parsed: MsgResponse = serde_json::from_str(&resp)?;
    if !parsed.ok {
        anyhow::bail!(
            "{}",
            parsed.error.unwrap_or_else(|| "unknown error".to_string())
        );
    }
    Ok(())
}

/// Send one leg of an exchange via the running daemon's IPC socket.
/// The receiving daemon surfaces an `exchange` (or `exchange_progress`) event; this
/// command itself only confirms the send (or reports a rate-limit /
/// unknown-participant / oversize error).
async fn exchange(opts: ExchangeOpts) -> Result<()> {
    let ExchangeOpts {
        swarm,
        nickname,
        to,
        exchange_id,
        kind,
        phase,
        text,
    } = opts;
    let cmd = IpcCommand::Exchange {
        swarm,
        to,
        exchange_id,
        kind,
        phase,
        body: text,
    };
    let resp = ipc::send(&cmd, &nickname).await?;
    let id = finish_send(&resp, "exchange")?;
    let out = Output::new(OutputMode::Human, false, None);
    out.msg_posted(&id);
    Ok(())
}

/// Query the running daemon's live participant roster. Always emits the
/// raw IPC JSON (`{ok, participants, count}`), like `poll`.
async fn peers(opts: PeersOpts) -> Result<()> {
    let PeersOpts { swarm, nickname } = opts;
    let cmd = IpcCommand::Peers { swarm };
    let resp = ipc::send(&cmd, &nickname).await?;
    println!("{resp}");
    Ok(())
}

/// Read or change the swarm's shared state via the running daemon. Emits the
/// raw IPC JSON response — `{ok,...}` for `patch`, `{ok,document}` for `get`.
async fn state(opts: StateOpts) -> Result<()> {
    let (cmd, nickname) = match opts.action {
        StateAction::Patch {
            swarm,
            nickname,
            patch,
        } => {
            let op_array: serde_json::Value = serde_json::from_str(&patch).map_err(|error| {
                anyhow::anyhow!("--patch must be a JSON array of RFC 6902 ops: {error}")
            })?;
            (
                IpcCommand::StatePatch {
                    swarm,
                    patch: op_array,
                },
                nickname,
            )
        }
        StateAction::Get { swarm, nickname } => (IpcCommand::StateGet { swarm }, nickname),
    };
    let resp = ipc::send(&cmd, &nickname).await?;
    println!("{resp}");
    Ok(())
}

/// Block until the daemon's `--state-file` reports it is *freshly* serving
/// (the `ready` flag is `true` and `last_updated` is recent), then exit 0.
/// A pure gate — prints nothing; the caller reads the identity from the same
/// file. Times out non-zero.
///
/// Robustness: a missing, unreadable, malformed, or stale-but-not-yet-ready
/// file is "not ready yet, keep polling" — only the deadline fails the gate.
/// This tolerates a half-written or old-schema file the daemon is about to
/// atomically overwrite, and the freshness check rejects a `ready: true`
/// left behind by a prior daemon that was killed with SIGKILL.
async fn ready(opts: ReadyOpts) -> Result<()> {
    let ReadyOpts {
        state_file,
        timeout_secs,
    } = opts;
    // Saturating add: an absurd `--timeout-secs` must not panic the gate
    // (`Instant + Duration` panics on overflow); clamp to a far-future
    // deadline instead.
    let now = tokio::time::Instant::now();
    let deadline = now
        .checked_add(std::time::Duration::from_secs(timeout_secs))
        .unwrap_or_else(|| {
            now + std::time::Duration::from_secs(crate::util::tuning::READY_MAX_SECS)
        });
    loop {
        // Read off the runtime's blocking pool: a `--state-file` on a hung
        // mount must not block a worker thread. A read error (malformed/torn/
        // old-schema file) is treated as not-ready, not a hard failure — the
        // daemon may be mid-overwrite. A *persistent* error (e.g. a bad path)
        // can't self-heal and just spins to the deadline, so log it so the
        // cause is recoverable.
        let path = state_file.clone();
        let read =
            tokio::task::spawn_blocking(move || crate::daemon::state_file::read_snapshot(&path))
                .await?;
        match read {
            Ok(Some(snapshot)) if snapshot.ready && ready_is_fresh(snapshot.last_updated) => {
                return Ok(());
            }
            Ok(_) => {}
            Err(error) => {
                tracing::debug!(%error, path = %state_file.display(), "ahsw ready: state-file read failed; retrying");
            }
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "daemon at {} not ready within {timeout_secs}s",
                state_file.display()
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(
            crate::util::tuning::READY_POLL_INTERVAL_MS,
        ))
        .await;
    }
}

/// True when a `ready: true` state-file write is recent enough to trust —
/// `last_updated` within `READY_FRESH_SECS` of now in *either* direction. A
/// live daemon refreshes `last_updated` every `STATE_REFRESH_SECS`, so a value
/// older than the window means the writer is gone (e.g. a leftover file from a
/// daemon killed by SIGKILL). A value far in the *future* is equally
/// untrustworthy — a stale file whose age only looks recent because the wall
/// clock stepped backward (NTP correction, VM restore) — so it is rejected
/// too. `clock::unix_secs` is non-negative, so the `i64` math below cannot
/// underflow into the past.
fn ready_is_fresh(last_updated: u64) -> bool {
    let now = crate::util::clock::unix_secs();
    let last_updated = i64::try_from(last_updated).unwrap_or(i64::MAX);
    let skew = now - last_updated; // >0: file is in the past; <0: in the future
    let window = i64::try_from(crate::util::tuning::READY_FRESH_SECS).unwrap_or(i64::MAX);
    skew.abs() <= window
}
