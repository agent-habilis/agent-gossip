//! The `ah-s` command-line interface: the clap-derived argument shape
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

mod agent;
mod args;
mod discover;
mod setup;
mod status;

pub(crate) use args::Cli;
use args::{Commands, CreateOpts, MsgOpts, PingOpts, PollOpts, SharedServerOpts};

/// `join` has no `--public`/`--name`: both are encoded in the `ahs…`
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
        Commands::Create { opts } => {
            crate::util::tuning::init(opts.shared.tuning());
            create(opts).await
        }
        Commands::Join { opts } => {
            reject_id_encoded_flag("--public", opts.public)?;
            reject_id_encoded_flag("--name", opts.name.is_some())?;
            crate::util::tuning::init(opts.shared.tuning());
            join(opts.swarm, opts.nickname, opts.shared).await
        }
        Commands::Msg { opts } => msg(opts).await,
        Commands::Poll { opts } => poll(opts).await,
        Commands::Ping { opts } => ping(opts).await,
        Commands::Discover { opts } => {
            crate::util::tuning::init(opts.shared.tuning());
            discover::discover(opts).await
        }
        Commands::Mcp => crate::mcp::run().await,
        // The manual is embedded at compile time (`include_str!`), so the
        // binary documents itself with no repo checkout.
        Commands::Man => {
            print!("{}", include_str!("../../docs/manual.txt"));
            Ok(())
        }
        // Embedded integration artifacts written to the agents' skills dirs —
        // self-contained, no repo checkout needed (like `Man`).
        Commands::Setup { execute, agents } => setup::setup(execute, &agents),
        Commands::Teardown { execute, agents } => setup::teardown(execute, &agents),
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
    let mut cfg = setup_swarm(
        kind,
        author,
        !shared.no_interactive,
        shared.max_peers,
        shared.state_file,
        out,
    )
    .await?;
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

/// Join an existing swarm by its identifier (ahs...), a domain, or a
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
    let parsed: MsgResponse = serde_json::from_str(&resp)?;

    // A rate-limited send is a deliberate drop, not a failure — surface it
    // distinctly (still non-zero, so scripts see the message wasn't sent).
    if parsed.rate_limited {
        anyhow::bail!("rate limit exceeded — message not sent");
    }
    if !parsed.ok {
        anyhow::bail!(
            "{}",
            parsed.error.unwrap_or_else(|| "unknown error".to_string())
        );
    }

    let id = parsed
        .id
        .ok_or_else(|| anyhow::anyhow!("msg response missing id"))?;
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
        output: _,
    } = opts;
    let cmd = IpcCommand::Poll { swarm, after };

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
