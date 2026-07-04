//! The `agent-gossip` command-line interface: the clap-derived argument shape
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

mod a2a_discover;
pub(crate) mod agent;
mod args;
mod discover;
mod doctor;
mod password;
mod picker;
mod plug;
mod session;

pub(crate) use args::Cli;
use args::{
    A2aAction, Commands, CreateOpts, ForumOpts, MetaAction, MetaOpts, OutputFormat, PeersOpts,
    PingOpts, PollOpts, ReadyOpts, SharedServerOpts, StateAction, StateOpts, TopologyOpts,
};

/// `join` has no `--public`/`--name`: both are encoded in the `💬…`
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
        Commands::Leave { opts } => session::leave(opts).await,
        Commands::Session { opts } => session::session(opts).await,
        Commands::Poll { opts } => poll(opts).await,
        Commands::Ping { opts } => ping(opts).await,
        Commands::A2a { opts } => Box::pin(a2a(opts.action)).await,
        Commands::Peers { opts } => peers(opts).await,
        // Boxed like the event-loop futures above: the discover arm holds a
        // picker + connect chain that puts it over clippy's 16 KiB
        // `large_futures` budget.
        Commands::State { opts } => state(opts).await,
        Commands::Meta { opts } => meta(opts).await,
        Commands::Topology { opts } => topology_cmd(opts).await,
        Commands::Ready { opts } => ready(opts).await,
        Commands::Discover { opts } => {
            crate::util::tuning::init(opts.shared.tuning());
            Box::pin(discover::discover(opts)).await
        }
        Commands::Mcp {
            directory_private,
            ping_window_secs,
            longpoll_max_ms,
        } => {
            // The MCP server holds no `SharedServerOpts`; install just the
            // hidden knobs the suite varies (loopback directory, short ping
            // window, short long-poll park) over the production defaults.
            crate::util::tuning::init(crate::util::tuning::Tuning {
                ping_window_secs,
                longpoll_max_ms,
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
    // stay hermetic. `agent-gossip status` is the on-demand counterpart.
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
        shared.a2a_serve,
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
        // `--no-gossip` is a swarm-wide characteristic baked into the id, so
        // every joiner inherits it from the ticket alone.
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

/// Join an existing swarm by its identifier (💬...), a domain, or a
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
#[expect(
    clippy::too_many_lines,
    reason = "flat dispatch over the a2a subcommand: the tunnel arms (expose/connect/discover) and the messaging arms (call/status/artifact) are each self-contained, so splitting would just scatter the one match"
)]
async fn a2a(action: A2aAction) -> Result<()> {
    match action {
        A2aAction::Expose {
            to,
            lookups,
            advertise,
            password,
            loopback,
            output,
        } => {
            let json = matches!(output, OutputFormat::Json);
            let password = password::resolve_password(password, /* confirm */ true, json)?;
            let advertise = crate::protocol::swarm::DirectorySelection::from_flag(advertise);
            Box::pin(crate::a2a::expose(
                &to,
                lookups.to_set(),
                advertise,
                loopback,
                json,
                password,
            ))
            .await
        }
        A2aAction::Connect {
            ticket,
            port,
            password,
            output,
        } => {
            let json = matches!(output, OutputFormat::Json);
            // Prompt when the ticket needs a password and none was passed
            // (mirrors the swarm-join UX); otherwise resolve the given flag.
            let password = match password {
                None if crate::a2a::ticket_requires_password(&ticket) => {
                    Some(password::require_password(json, "ticket")?)
                }
                other => password::resolve_password(other, /* confirm */ false, json)?,
            };
            crate::a2a::connect(&ticket, port, json, password).await
        }
        A2aAction::Discover {
            directory,
            port,
            lookups,
            password,
            output,
        } => {
            let json = matches!(output, OutputFormat::Json);
            Box::pin(a2a_discover::discover(
                directory,
                port,
                lookups.to_set(),
                password,
                json,
            ))
            .await
        }
        A2aAction::Call {
            swarm,
            nickname,
            to,
            method,
            text,
            task_id,
            params,
            timeout_secs,
        } => {
            // No peer + SendMessage = swarm broadcast chat (fire-and-forget).
            if to.is_none() && method == "SendMessage" {
                let text = text.ok_or_else(|| {
                    anyhow::anyhow!("broadcast SendMessage needs --text (or a --to peer)")
                })?;
                let body = crate::protocol::MessageBody::new(text)
                    .map_err(|error| anyhow::anyhow!("{error}"))?;
                let resp = ipc::send(&IpcCommand::Msg { swarm, body }, &nickname).await?;
                let id = finish_send(&resp, "message")?;
                Output::new(OutputMode::Human, false, None).msg_posted(&id);
                return Ok(());
            }
            let to = to.ok_or_else(|| {
                anyhow::anyhow!(
                    "--to <peer> is required for an RPC call (only broadcast SendMessage omits it)"
                )
            })?;
            // Compose params from --text/--task-id sugar, unless raw --params given.
            let params: serde_json::Value = match params {
                Some(raw) => serde_json::from_str(&raw)
                    .map_err(|error| anyhow::anyhow!("--params must be valid JSON: {error}"))?,
                None => compose_a2a_params(&method, &swarm, text.as_deref(), task_id.as_ref()),
            };
            let cmd = IpcCommand::A2aCall {
                swarm,
                to,
                method,
                params,
                timeout_secs,
            };
            let resp = ipc::send(&cmd, &nickname).await?;
            println!("{resp}");
            let parsed: serde_json::Value = serde_json::from_str(&resp)?;
            if !parsed["error"].is_null() {
                std::process::exit(1);
            }
            Ok(())
        }
        A2aAction::Status {
            swarm,
            nickname,
            task_id,
            state,
            note,
        } => {
            let cmd = IpcCommand::A2aStatus {
                swarm,
                task_id,
                state,
                note,
            };
            let resp = ipc::send(&cmd, &nickname).await?;
            let id = finish_send(&resp, "task status")?;
            Output::new(OutputMode::Human, false, None).msg_posted(&id);
            Ok(())
        }
        A2aAction::Artifact {
            swarm,
            nickname,
            task_id,
            text,
            file,
            file_name,
            file_mime,
        } => {
            if text.is_none() && file.is_none() {
                anyhow::bail!("provide --text and/or --file");
            }
            // Default the advertised filename to the file's own basename.
            let file_name = file_name.or_else(|| {
                file.as_ref()
                    .and_then(|path| path.file_name())
                    .and_then(|name| name.to_str())
                    .map(str::to_owned)
            });
            let cmd = IpcCommand::A2aArtifact {
                swarm,
                task_id,
                text: text.unwrap_or_default(),
                file,
                file_name,
                file_mime,
            };
            let resp = ipc::send(&cmd, &nickname).await?;
            let id = finish_send(&resp, "task artifact")?;
            Output::new(OutputMode::Human, false, None).msg_posted(&id);
            Ok(())
        }
        A2aAction::Fetch { ticket, password } => {
            let ticket = crate::blob::BlobTicket::decode(&ticket)?;
            let password = password.map(crate::protocol::crypto::Password::new);
            let mut stdout = tokio::io::stdout();
            crate::blob::fetch(&ticket, &mut stdout, password).await?;
            Ok(())
        }
    }
}

fn compose_a2a_params(
    method: &str,
    swarm: &crate::protocol::SwarmId,
    text: Option<&str>,
    task_id: Option<&crate::a2a::TaskId>,
) -> serde_json::Value {
    match method {
        "SendMessage" | "SendStreamingMessage" => {
            let message =
                crate::a2a::gossip::send_message_payload(swarm, task_id, text.unwrap_or_default());
            serde_json::json!({ "message": message })
        }
        "GetTask" | "CancelTask" | "SubscribeToTask" => task_id.map_or_else(
            || serde_json::json!({}),
            |id| serde_json::json!({ "id": id }),
        ),
        _ => serde_json::json!({}),
    }
}

/// Query the running daemon's live participant roster. Always emits the
/// raw IPC JSON (`{ok, participants, participant_count}`), like `poll`.
async fn poll(opts: PollOpts) -> Result<()> {
    let PollOpts {
        swarm,
        nickname,
        after,
        long,
        output: _,
    } = opts;
    let cmd = IpcCommand::Poll { swarm, after, long };

    loop {
        let started = tokio::time::Instant::now();
        let resp = ipc::send(&cmd, &nickname).await?;
        // An empty response is the daemon closing the connection without
        // writing (shutdown mid-park) — `round_trip` maps that EOF to `""`.
        // Surface it instead of printing a blank line / retrying forever.
        anyhow::ensure!(
            !resp.is_empty(),
            "swarm daemon closed the connection without a response (shutting down?)"
        );
        if !(long && resp == "[]") {
            println!("{resp}");
            return Ok(());
        }
        // Both empty paths render exactly `[]`; an error response is a JSON
        // object, so it prints and exits above. Floor the cycle so a daemon
        // degrading long reads to immediate empties (waiter registry full)
        // can't spin this loop hot — the unchanged cursor re-reads anything
        // that lands during the sleep.
        let min_cycle =
            std::time::Duration::from_millis(crate::util::tuning::POLL_LONG_MIN_CYCLE_MS);
        if let Some(remaining) = min_cycle.checked_sub(started.elapsed()) {
            tokio::time::sleep(remaining).await;
        }
    }
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

/// Print the whisper routing topology (assembled mesh graph) from the running
/// daemon, as JSON. Backs the `/swarm:topology` render.
async fn topology_cmd(opts: TopologyOpts) -> Result<()> {
    let cmd = IpcCommand::Topology { swarm: opts.swarm };
    let resp = ipc::send(&cmd, &opts.nickname).await?;
    println!("{resp}");
    let parsed: MsgResponse = serde_json::from_str(&resp)?;
    if !parsed.ok {
        std::process::exit(1);
    }
    Ok(())
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
                tracing::debug!(%error, path = %state_file.display(), "agent-gossip ready: state-file read failed; retrying");
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

/// Print the session identity as a JSON object for `agent-gossip ready --output json`,
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
