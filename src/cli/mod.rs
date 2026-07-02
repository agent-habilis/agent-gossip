//! The `ahsw` command-line interface: the clap-derived argument shape
//! lives in [`args`], the live `discover` picker in [`discover`], and the
//! per-subcommand handlers + [`dispatch`] here. `lib.rs::run_cli` parses
//! argv and calls `dispatch`; each handler is the thin glue between the
//! parsed args and the daemon / IPC / embed layers it drives.

use anyhow::Result;
use serde::Deserialize;

use crate::daemon::run as run_event_loop;
use crate::daemon::setup::{SetupKind, setup_swarm};
use crate::daemon::{CreateParams, ForumParams, JoinParams, Resolved};
use crate::embed::spawn_advertiser;
use crate::output::{Output, OutputMode};
use crate::protocol::swarm::{Swarm, SwarmConfig, SwarmName, resolve_lookups};
use crate::protocol::{MessageId, Nickname};
use crate::resolver::JoinTarget;
use crate::transport::ipc::{self, IpcCommand};

pub(crate) mod agent;
mod args;
mod discover;
mod doctor;
mod password;
mod picker;
mod plug;
mod ticket_discover;

pub(crate) use args::Cli;
use args::{
    Commands, CreateOpts, FileAction, ForumOpts, MetaAction, MetaOpts, MountAction, MsgOpts,
    OutputFormat, PeersOpts, PingOpts, PipeAction, PollOpts, PortAction, ReadyOpts, ShAction,
    SharedServerOpts, StateAction, StateOpts, TaskOpts,
};

/// `join` has no `--public`/`--name`: both are encoded in the `🐝…`
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
            Box::pin(join(opts.swarm, opts.nickname, opts.password, opts.shared)).await
        }
        Commands::Forum { opts } => {
            crate::util::tuning::init(opts.shared.tuning());
            Box::pin(forum(opts)).await
        }
        Commands::Msg { opts } => msg(opts).await,
        Commands::Poll { opts } => poll(opts).await,
        Commands::Ping { opts } => ping(opts).await,
        Commands::Task { opts } => task(opts).await,
        Commands::Peers { opts } => peers(opts).await,
        // Boxed like the event-loop futures above: the discover arms hold a
        // picker + connect chain that puts these over clippy's 16 KiB
        // `large_futures` budget.
        Commands::Pipe { action } => Box::pin(pipe(action)).await,
        Commands::Port { action } => Box::pin(port(action)).await,
        Commands::File { action } => Box::pin(file(action)).await,
        Commands::Sh { action } => sh(action).await,
        Commands::Mount {
            action,
            ticket,
            mountpoint,
            no_mount,
            output,
        } => mount(action, ticket, mountpoint, no_mount, output).await,
        Commands::State { opts } => state(opts).await,
        Commands::Meta { opts } => meta(opts).await,
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
        Commands::Doctor { opts } => doctor::run(opts).await,
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
        SetupKind::Join { .. } | SetupKind::Forum { .. } => None,
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
    let password = password::resolve_password(
        opts.password.clone(),
        /* confirm */ true,
        opts.shared.no_prompt(),
    )?;
    let config = SwarmConfig {
        lookups: resolve_lookups(opts.public, opts.lookups.to_set()),
        // The verifier is baked in at setup: its salt is the seed, which is
        // minted there. The flag's presence is all `resolve` needs.
        password: None,
    };
    // `resolve` validates `--advertise` against the config (never a silent
    // no-op) before any setup work.
    let resolved = CreateParams {
        name: opts.name.unwrap_or_else(SwarmName::random),
        nickname: opts.nickname,
        config,
        advertise,
        password,
    }
    .resolve()?;
    run_session(resolved, opts.shared).await
}

/// Join an existing swarm by its identifier (🐝...), a domain, or a
/// supported git repo URL. The swarm's config (lookups) is decoded from
/// the id — `join` takes no lookup flags.
#[expect(
    clippy::option_option,
    reason = "clap optional-value flag: absent/bare/valued are three distinct password states"
)]
async fn join(
    target: JoinTarget,
    nickname: Option<Nickname>,
    password_flag: Option<Option<String>>,
    shared: SharedServerOpts,
) -> Result<()> {
    let no_prompt = shared.no_prompt();
    let mut password =
        password::resolve_password(password_flag, /* confirm */ false, no_prompt)?;
    // A protected id with the flag absent still prompts on a TTY — the
    // operator pasted an id and shouldn't need to know about the flag.
    if password.is_none()
        && let JoinTarget::Swarm(id) = &target
        && id
            .as_str()
            .parse::<Swarm>()
            .is_ok_and(|swarm| swarm.requires_password())
    {
        password = Some(password::require_password(no_prompt, "swarm")?);
    }
    // `join` never advertises — that is a create-time decision.
    let resolved = JoinParams {
        target,
        nickname,
        password,
    }
    .resolve()?;
    run_session(resolved, shared).await
}

/// Join a public swarm derived deterministically from a shared string. The
/// seed, name, and (always-public) config are all derived from the string, so
/// the same string joins the same forum on any machine — no id to share.
async fn forum(opts: ForumOpts) -> Result<()> {
    let resolved = ForumParams {
        string: opts.string,
        nickname: opts.nickname,
    }
    .resolve()?;
    run_session(resolved, opts.shared).await
}

#[derive(Deserialize)]
struct MsgResponse {
    ok: bool,
    id: Option<MessageId>,
    error: Option<String>,
}

/// Reduce an IPC send response (`msg` / `task`, same `{ok,id,error}`
/// shape) to the new message id, or a descriptive error. `what` names the
/// operation for the missing-id message.
fn finish_send(resp: &str, what: &str) -> Result<MessageId> {
    let parsed: MsgResponse = serde_json::from_str(resp)?;
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
    let PingOpts {
        swarm,
        nickname,
        output: _,
    } = opts;
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

/// Send one leg of a task via the running daemon's IPC socket.
/// The receiving daemon surfaces a `task` (or `task_progress`) event; this
/// command itself only confirms the send (or reports an
/// unknown-participant / oversize error).
async fn task(opts: TaskOpts) -> Result<()> {
    let TaskOpts {
        swarm,
        nickname,
        to,
        task_id,
        phase,
        text,
    } = opts;
    let cmd = IpcCommand::Task {
        swarm,
        to,
        task_id,
        phase,
        body: text,
    };
    let resp = ipc::send(&cmd, &nickname).await?;
    let id = finish_send(&resp, "task")?;
    let out = Output::new(OutputMode::Human, false, None);
    out.msg_posted(&id);
    Ok(())
}

/// Query the running daemon's live participant roster. Always emits the
/// raw IPC JSON (`{ok, participants, participant_count}`), like `poll`.
async fn peers(opts: PeersOpts) -> Result<()> {
    let PeersOpts {
        swarm,
        nickname,
        output: _,
    } = opts;
    let cmd = IpcCommand::Peers { swarm };
    let resp = ipc::send(&cmd, &nickname).await?;
    println!("{resp}");
    Ok(())
}

/// `ahsw pipe` — a standalone, off-gossip direct byte stream (no daemon).
/// `listen` reads stdin and prints the connect command on stdout; `connect`
/// redeems one and streams the peer's bytes to stdout.
async fn pipe(action: PipeAction) -> Result<()> {
    match action {
        PipeAction::Listen {
            swarm,
            lookups,
            advertise,
            tuning,
            throttle,
            output,
            password,
            follow,
        } => {
            crate::util::tuning::init(tuning.tuning());
            let json = matches!(output, OutputFormat::Json);
            let password = password::resolve_password(password, /* confirm */ true, json)?;
            crate::pipe::listen(
                swarm.as_ref().map(crate::protocol::SwarmId::as_str),
                lookups.to_set(),
                crate::protocol::swarm::DirectorySelection::from_flag(advertise),
                throttle,
                json,
                follow,
                password,
            )
            .await
        }
        PipeAction::Connect {
            ticket,
            throttle,
            password,
        } => {
            let password =
                consumer_password(password, &ticket, crate::pipe::ticket_requires_password)?;
            crate::pipe::connect(&ticket, throttle, password).await
        }
        PipeAction::Discover {
            name,
            lookups,
            tuning,
            throttle,
            password,
            output,
        } => {
            crate::util::tuning::init(tuning.tuning());
            let json = matches!(output, OutputFormat::Json);
            let password = password::resolve_password(password, /* confirm */ false, json)?;
            match ticket_discover::discover_ticket(
                name,
                lookups.to_set(),
                crate::protocol::token::TokenType::Pipe,
                json,
            )
            .await?
            {
                Some(ticket) => {
                    let password = match password {
                        None if crate::pipe::ticket_requires_password(&ticket) => {
                            Some(password::require_password(false, "ticket")?)
                        }
                        other => other,
                    };
                    ticket_discover::interruptible(crate::pipe::connect(
                        &ticket, throttle, password,
                    ))
                    .await
                }
                None => Ok(()),
            }
        }
        PipeAction::Bench {
            ticket,
            serve,
            swarm,
            lookups,
            budget,
            pings,
            password,
            output,
        } => {
            let json = matches!(output, OutputFormat::Json);
            // clap enforces the producer/consumer split: `--serve`/`--swarm`
            // and the lookup flags conflict with a ticket, `--budget`/`--pings`
            // require one. So a ticket means consumer, its absence means
            // producer.
            match ticket {
                None => {
                    let password =
                        password::resolve_password(password, /* confirm */ true, json)?;
                    crate::pipe::listen_bench(
                        swarm.as_ref().map(crate::protocol::SwarmId::as_str),
                        lookups.to_set(),
                        serve,
                        json,
                        password,
                    )
                    .await
                }
                Some(ticket) => {
                    let password = consumer_password(
                        password,
                        &ticket,
                        crate::pipe::ticket_requires_password,
                    )?;
                    let opts = crate::pipe::BenchOpts {
                        budget: budget.unwrap_or_default(),
                        pings: pings.unwrap_or(20),
                    };
                    crate::pipe::connect_bench(&ticket, opts, json, password).await
                }
            }
        }
    }
}

/// Resolve a consumer-side `--password` flag, prompting when the flag is
/// absent but `ticket` decodes as password-protected (per `requires` — each
/// transfer kind checks its own ticket codec). The prompt itself fails
/// cleanly without a TTY, telling the caller to pass `--password=<pw>`.
#[expect(
    clippy::option_option,
    reason = "clap optional-value flag: absent/bare/valued are three distinct password states"
)]
fn consumer_password(
    flag: Option<Option<String>>,
    ticket: &str,
    requires: impl Fn(&str) -> bool,
) -> Result<Option<crate::protocol::crypto::Password>> {
    let password = password::resolve_password(flag, /* confirm */ false, false)?;
    match password {
        None if requires(ticket) => Ok(Some(password::require_password(false, "ticket")?)),
        other => Ok(other),
    }
}

/// `ahsw port` — a standalone, off-gossip TCP forward (no daemon). `listen`
/// exposes a local port and prints the connect command on stdout; `connect`
/// redeems a ticket and forwards each local connection to the producer.
async fn port(action: PortAction) -> Result<()> {
    match action {
        PortAction::Listen {
            ports,
            swarm,
            lookups,
            advertise,
            tuning,
            password,
            output,
        } => {
            crate::util::tuning::init(tuning.tuning());
            let json = matches!(output, OutputFormat::Json);
            let password = password::resolve_password(password, /* confirm */ true, json)?;
            crate::port::listen(
                swarm.as_ref().map(crate::protocol::SwarmId::as_str),
                lookups.to_set(),
                crate::protocol::swarm::DirectorySelection::from_flag(advertise),
                &ports,
                json,
                password,
            )
            .await
        }
        PortAction::Connect {
            ticket,
            ports,
            password,
            output,
        } => {
            let password =
                consumer_password(password, &ticket, crate::port::ticket_requires_password)?;
            crate::port::connect(
                &ticket,
                &ports,
                matches!(output, OutputFormat::Json),
                password,
            )
            .await
        }
        PortAction::Discover {
            name,
            ports,
            lookups,
            tuning,
            password,
            output,
        } => {
            crate::util::tuning::init(tuning.tuning());
            let json = matches!(output, OutputFormat::Json);
            let password = password::resolve_password(password, /* confirm */ false, json)?;
            match ticket_discover::discover_ticket(
                name,
                lookups.to_set(),
                crate::protocol::token::TokenType::Port,
                json,
            )
            .await?
            {
                Some(ticket) => {
                    // No explicit mappings ⇒ forward every advertised port to
                    // the same local port.
                    let mappings = if ports.is_empty() {
                        crate::port::identity_mappings(&ticket)?
                    } else {
                        ports
                    };
                    let password = match password {
                        None if crate::port::ticket_requires_password(&ticket) => {
                            Some(password::require_password(false, "ticket")?)
                        }
                        other => other,
                    };
                    ticket_discover::interruptible(crate::port::connect(
                        &ticket, &mappings, json, password,
                    ))
                    .await
                }
                None => Ok(()),
            }
        }
    }
}

/// `ahsw file` — a standalone, off-gossip file/folder transfer (no daemon).
/// `send` serves a path and prints the `get` command on stdout; `get` redeems a
/// ticket and receives the tree, fetching only what has changed.
async fn file(action: FileAction) -> Result<()> {
    match action {
        FileAction::Send {
            path,
            swarm,
            lookups,
            advertise,
            tuning,
            throttle,
            password,
            output,
        } => {
            crate::util::tuning::init(tuning.tuning());
            let json = matches!(output, OutputFormat::Json);
            let password = password::resolve_password(password, /* confirm */ true, json)?;
            crate::file::send(
                swarm.as_ref().map(crate::protocol::SwarmId::as_str),
                lookups.to_set(),
                crate::protocol::swarm::DirectorySelection::from_flag(advertise),
                &path,
                throttle,
                json,
                password,
            )
            .await
        }
        FileAction::Get {
            ticket,
            out,
            throttle,
            password,
            output,
        } => {
            let password =
                consumer_password(password, &ticket, crate::file::ticket_requires_password)?;
            crate::file::get(
                &ticket,
                out.as_deref(),
                throttle,
                matches!(output, OutputFormat::Json),
                password,
            )
            .await
        }
        FileAction::Discover {
            name,
            lookups,
            tuning,
            out,
            throttle,
            password,
            output,
        } => {
            crate::util::tuning::init(tuning.tuning());
            let json = matches!(output, OutputFormat::Json);
            let password = password::resolve_password(password, /* confirm */ false, json)?;
            match ticket_discover::discover_ticket(
                name,
                lookups.to_set(),
                crate::protocol::token::TokenType::File,
                json,
            )
            .await?
            {
                Some(ticket) => {
                    let password = match password {
                        None if crate::file::ticket_requires_password(&ticket) => {
                            Some(password::require_password(false, "ticket")?)
                        }
                        other => other,
                    };
                    ticket_discover::interruptible(crate::file::get(
                        &ticket,
                        out.as_deref(),
                        throttle,
                        json,
                        password,
                    ))
                    .await
                }
                None => Ok(()),
            }
        }
    }
}

async fn sh(action: ShAction) -> Result<()> {
    match action {
        ShAction::Listen {
            swarm,
            lookups,
            output,
            write,
            command,
            cols,
            rows,
            password,
        } => {
            let json = matches!(output, OutputFormat::Json);
            let password = password::resolve_password(password, /* confirm */ true, json)?;
            crate::sh::listen(
                swarm.as_ref().map(crate::protocol::SwarmId::as_str),
                lookups.to_set(),
                json,
                write,
                command.as_deref(),
                cols,
                rows,
                password,
            )
            .await
        }
        ShAction::Connect { ticket, password } => {
            let password =
                consumer_password(password, &ticket, crate::sh::ticket_requires_password)?;
            crate::sh::connect(&ticket, password).await
        }
    }
}

async fn mount(
    action: Option<MountAction>,
    ticket: Option<String>,
    mountpoint: Option<std::path::PathBuf>,
    no_mount: bool,
    output: OutputFormat,
) -> Result<()> {
    let json = matches!(output, OutputFormat::Json);
    if let Some(MountAction::Serve {
        dir,
        swarm,
        lookups,
        output: serve_output,
    }) = action
    {
        return crate::mount::serve(
            swarm.as_ref().map(crate::protocol::SwarmId::as_str),
            lookups.to_set(),
            &dir,
            matches!(serve_output, OutputFormat::Json),
        )
        .await;
    }
    // The bare form: both positionals are optional at the clap layer (the
    // `serve` subcommand shares the slot), so require them here.
    let (Some(ticket), Some(mountpoint)) = (ticket, mountpoint) else {
        anyhow::bail!("usage: ahsw mount <🐝…> <mountpoint>, or ahsw mount serve <dir>");
    };
    crate::mount::attach(&ticket, &mountpoint, no_mount, json).await
}

/// Read or change the swarm's shared state via the running daemon. Emits the
/// raw IPC JSON response — `{ok,...}` for `merge`, `{ok,document}` for `get`.
async fn state(opts: StateOpts) -> Result<()> {
    let (cmd, nickname) = match opts.action {
        StateAction::Merge {
            swarm,
            nickname,
            merge,
        } => {
            let merge_doc: serde_json::Value = serde_json::from_str(&merge).map_err(|error| {
                anyhow::anyhow!("--merge must be valid JSON (an RFC 7386 merge patch): {error}")
            })?;
            (
                IpcCommand::StateMerge {
                    swarm,
                    merge: merge_doc,
                },
                nickname,
            )
        }
        StateAction::Get { swarm, nickname } => (IpcCommand::StateGet { swarm }, nickname),
    };
    let resp = ipc::send(&cmd, &nickname).await?;
    println!("{resp}");
    // A rejected merge must not exit 0: a shell-driven agent
    // reads the exit code to tell an applied change from a rejected one, and an
    // `{"ok":false}` that exits 0 reads as success → silent desync. The raw JSON
    // is already printed above for `--output json` consumers; the exit code is
    // the scriptable signal. (`get` returns `ok:true`, so it stays exit 0.)
    let parsed: MsgResponse = serde_json::from_str(&resp)?;
    if !parsed.ok {
        std::process::exit(1);
    }
    Ok(())
}

/// Read or change the swarm's `meta` channel via the running daemon — the
/// independent counterpart of [`state`]. Same raw-JSON + exit-code contract.
async fn meta(opts: MetaOpts) -> Result<()> {
    let (cmd, nickname) = match opts.action {
        MetaAction::Merge {
            swarm,
            nickname,
            merge,
        } => {
            let merge_doc: serde_json::Value = serde_json::from_str(&merge).map_err(|error| {
                anyhow::anyhow!("--merge must be valid JSON (an RFC 7386 merge patch): {error}")
            })?;
            (
                IpcCommand::MetaMerge {
                    swarm,
                    merge: merge_doc,
                },
                nickname,
            )
        }
        MetaAction::Get { swarm, nickname } => (IpcCommand::MetaGet { swarm }, nickname),
    };
    let resp = ipc::send(&cmd, &nickname).await?;
    println!("{resp}");
    let parsed: MsgResponse = serde_json::from_str(&resp)?;
    if !parsed.ok {
        std::process::exit(1);
    }
    Ok(())
}

/// Block until the daemon's `--state-file` reports it is *freshly* serving
/// (the `ready` flag is `true` and `last_updated` is recent), then exit 0.
/// In `human` mode a pure gate — prints nothing; with `--output json` it
/// prints `{swarm,name,nickname}` on success, so the gate doubles as the
/// identity read instead of the caller parsing the file. Times out non-zero.
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
        output,
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
                if matches!(output, OutputFormat::Json) {
                    print_ready_identity(&state_file);
                }
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

/// Print the session identity as a JSON object for `ahsw ready --output json`,
/// omitting any field the state file lacks — so a degenerate (identity-less)
/// file yields `{}` rather than `{"swarm":null,…}` that a caller might splice
/// into the next command as the literal string "null".
fn print_ready_identity(state_file: &std::path::Path) {
    let identity = crate::daemon::state_file::read_identity(state_file);
    let mut obj = serde_json::Map::new();
    for (key, value) in [
        ("swarm", identity.swarm),
        ("name", identity.name),
        ("nickname", identity.nickname),
    ] {
        if let Some(value) = value {
            obj.insert(key.to_owned(), value.into());
        }
    }
    println!("{}", serde_json::Value::Object(obj));
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
