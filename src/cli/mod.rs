//! The `ahs` command-line interface: the clap-derived argument shape
//! lives in [`args`], the live `discover` picker in [`discover`], and the
//! per-subcommand handlers + [`dispatch`] here. `lib.rs::run_cli` parses
//! argv and calls `dispatch`; each handler is the thin glue between the
//! parsed args and the daemon / IPC / embed layers it drives.

use anyhow::Result;
use serde::Deserialize;

use crate::daemon::run as run_event_loop;
use crate::daemon::setup::{SetupKind, setup_swarm};
use crate::embed::spawn_advertiser;
use crate::output::{Output, OutputMode};
use crate::protocol::swarm::{
    DirectorySelection, LookupOpts, Swarm, SwarmMode, SwarmName, resolve_lookups,
    validate_advertise,
};
use crate::protocol::{MessageBody, MessageId, Nickname, SwarmId};
use crate::resolver;
use crate::transport::ipc::{self, IpcCommand};

mod args;
mod discover;

pub(crate) use args::Cli;
use args::{Commands, CreateOpts, JoinOpts, SharedServerOpts};

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
    match cli.command {
        Commands::Create { opts } => create(opts).await,
        Commands::Join {
            swarm,
            nickname,
            public,
            name,
            opts,
        } => {
            reject_id_encoded_flag("--public", public)?;
            reject_id_encoded_flag("--name", name.is_some())?;
            join(&swarm, nickname, opts).await
        }
        Commands::Msg {
            swarm,
            nickname,
            text,
            reply,
        } => msg(&swarm, &nickname, text, reply).await,
        Commands::Poll {
            swarm,
            nickname,
            after,
            output: _,
        } => poll(&swarm, &nickname, after).await,
        Commands::Ping { swarm, nickname } => ping(&swarm, &nickname).await,
        Commands::Discover { directory, opts } => discover::discover(directory, opts).await,
        Commands::Mcp => crate::mcp::run().await,
    }
}

/// Build the output sink, resolve the author, set up the swarm, and
/// run the event loop. The shared spine of `create` and `join`.
async fn run_session(
    kind: SetupKind,
    lookups: LookupOpts,
    shared: SharedServerOpts,
    nickname: Option<Nickname>,
    advertise: DirectorySelection,
) -> Result<()> {
    let author = nickname.unwrap_or_else(Nickname::random);
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
        lookups.clone(),
        out,
    )
    .await?;
    // Advertising (`create --advertise`): start the re-broadcast task on
    // the swarm's own lookups. The handle is held for the session's
    // lifetime — on the CLI the process exits (via signal) before it
    // would drop, which tears the task down.
    let _advertiser = advertise
        .directory()
        .map(|directory| spawn_advertiser(&mut cfg, directory, lookups));
    // First point where swarm id + nickname are known — attach the
    // buffered log sink here (see `logsink`).
    crate::logsink::attach(&cfg.swarm, &cfg.author);
    run_event_loop(cfg).await
}

/// Create a new swarm, print its identifier, and start listening.
async fn create(opts: CreateOpts) -> Result<()> {
    let mode = if opts.public {
        SwarmMode::Public
    } else {
        SwarmMode::Private
    };
    let lookups = resolve_lookups(mode, opts.shared.lookups.to_set())?;
    let advertise = opts.advertise_selection();
    // `--advertise` lists the swarm publicly, so it requires `--public`
    // (rejected before any setup work — never a silent no-op).
    validate_advertise(mode, &advertise)?;
    let name = opts.name.unwrap_or_else(SwarmName::random);
    let kind = SetupKind::Create { mode, name };
    run_session(kind, lookups, opts.shared, opts.nickname, advertise).await
}

/// Join an existing swarm by its identifier (ahs...), a domain, or a
/// supported git repo URL. The network mode is decoded from the id;
/// lookup flags are per-process (the joiner opts in itself).
async fn join(swarm_input: &str, nickname: Option<Nickname>, opts: JoinOpts) -> Result<()> {
    let swarm: Swarm = resolver::resolve(swarm_input).await?;
    let lookups = resolve_lookups(swarm.mode, opts.shared.lookups.to_set())?;
    // `join` never advertises — that is a create-time decision.
    run_session(
        SetupKind::Join { swarm },
        lookups,
        opts.shared,
        nickname,
        DirectorySelection::Unset,
    )
    .await
}

#[derive(Deserialize)]
struct MsgResponse {
    ok: bool,
    id: Option<String>,
    error: Option<String>,
    #[serde(default)]
    rate_limited: bool,
}

/// Post a message to a swarm via the running server's IPC socket.
async fn msg(
    swarm: &SwarmId,
    nickname: &Nickname,
    text: MessageBody,
    reply: Option<Nickname>,
) -> Result<()> {
    let cmd = IpcCommand::Msg {
        swarm: swarm.clone(),
        body: text,
        reply,
    };

    let resp = ipc::send(&cmd, nickname).await?;
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

    let id = parsed.id.unwrap_or_default();
    // `msg` has no `--output` flag — always the human confirmation.
    // No nickname is rendered here (only `message posted` + id).
    let out = Output::new(OutputMode::Human, false, None);
    out.msg_posted(&id);

    Ok(())
}

/// Retrieve buffered messages from a running swarm process via IPC.
async fn poll(swarm: &SwarmId, nickname: &Nickname, after: Option<MessageId>) -> Result<()> {
    let cmd = IpcCommand::Poll {
        swarm: swarm.clone(),
        after,
    };

    let resp = ipc::send(&cmd, nickname).await?;
    println!("{resp}");

    Ok(())
}

/// Arm an RTT round on the running daemon. Fire-and-forget: the daemon
/// acks immediately and emits the `ping_report` on its own
/// `--output json` stream once the collection window closes.
async fn ping(swarm: &SwarmId, nickname: &Nickname) -> Result<()> {
    let cmd = IpcCommand::Ping {
        swarm: swarm.clone(),
    };
    let resp = ipc::send(&cmd, nickname).await?;
    let parsed: MsgResponse = serde_json::from_str(&resp)?;
    if !parsed.ok {
        anyhow::bail!(
            "{}",
            parsed.error.unwrap_or_else(|| "unknown error".to_string())
        );
    }
    Ok(())
}
