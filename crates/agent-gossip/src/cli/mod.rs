//! The `agent-gossip` command-line interface: the clap-derived argument shape
//! lives in [`args`], the `discover` JSON stream in [`discover`], and the
//! per-subcommand handlers + [`dispatch`] here. `lib.rs::run_cli` parses
//! argv and calls `dispatch`; each handler is the thin glue between the
//! parsed args and the daemon / IPC / api layers it drives.

use anyhow::Result;
use serde::Deserialize;

use crate::a2a::ipc::IpcCommand;
use crate::api::spawn_advertiser;
use crate::output::{Output, OutputMode};
use agent_habilis_mesh::daemon::run as run_event_loop;
use agent_habilis_mesh::daemon::setup::{SetupKind, SetupParams, setup_mesh};
use agent_habilis_mesh::daemon::{CreateParams, JoinParams, Resolved, TopicParams};
use agent_habilis_mesh::protocol::mesh::{Mesh, MeshConfig, MeshName, resolve_lookups};
use agent_habilis_mesh::protocol::{MeshId, MessageId, Nickname};
use agent_habilis_mesh::resolver::JoinTarget;
use agent_habilis_mesh::transport::ipc;

mod a2a_discover;
pub(crate) mod agent;
mod args;
mod discover;
mod doctor;
mod password;
mod plug;
mod session;
mod signal;

pub(crate) use args::Cli;
use args::{
    A2aAction, Commands, CreateOpts, InviteOpts, MetaAction, MetaOpts, PeersOpts, PingOpts,
    PollOpts, ReadyOpts, SharedServerOpts, StateAction, StateOpts, TopicOpts, TopologyOpts,
};

/// `join` has no `--public`/`--name`: both are encoded in the `💬…`
/// identifier and auto-detected. Without this, clap rejects them with
/// a generic "unexpected argument" + a misleading "pass as a value"
/// tip; this gives the real reason instead.
fn reject_id_encoded_flag(flag: &str, present: bool) -> Result<()> {
    if present {
        anyhow::bail!(
            "`{flag}` is not valid for `join`: the mesh's network mode \
             and name are encoded in the mesh id and auto-detected. \
             Drop `{flag}` — `join` takes only the id and `--nickname`."
        );
    }
    Ok(())
}

/// Run the selected subcommand to completion.
///
/// # Errors
/// Propagates any error from the selected subcommand — mesh setup
/// failure, join timeout, IPC errors, invalid mesh-mode flags, etc.
pub(crate) async fn dispatch(cli: Cli) -> Result<()> {
    // Install the log-path config from the global flags before any
    // subcommand resolves its log file (the buffered sink flushes at
    // `logging::attach`, after this). Replaces the old AHS_LOG_DIR /
    // AHS_LOG_MAX_BYTES env reads.
    agent_habilis_mesh::util::logs::configure(agent_habilis_mesh::util::logs::LogConfig {
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
            agent_habilis_mesh::util::tuning::init(opts.shared.tuning.tuning());
            Box::pin(create(opts)).await
        }
        Commands::Join { opts } => {
            reject_id_encoded_flag("--public", opts.public)?;
            reject_id_encoded_flag("--name", opts.name.is_some())?;
            agent_habilis_mesh::util::tuning::init(opts.shared.tuning.tuning());
            Box::pin(join(opts.room, opts.nickname, opts.password, opts.shared)).await
        }
        Commands::Topic { opts } => {
            agent_habilis_mesh::util::tuning::init(opts.shared.tuning.tuning());
            Box::pin(topic(opts)).await
        }
        Commands::Leave { opts } => session::leave(opts).await,
        Commands::Session { opts } => session::session(opts).await,
        Commands::Poll { opts } => poll(opts).await,
        Commands::Ping { opts } => ping(opts).await,
        Commands::A2a { opts } => Box::pin(a2a(opts.action)).await,
        Commands::Peers { opts } => peers(opts).await,
        Commands::Invite { opts } => invite(opts).await,
        Commands::State { opts } => state(opts).await,
        Commands::Meta { opts } => meta(opts).await,
        Commands::Topology { opts } => topology_cmd(opts).await,
        Commands::Ready { opts } => ready(opts).await,
        Commands::Discover { opts } => {
            agent_habilis_mesh::util::tuning::init(opts.tuning.tuning());
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
            agent_habilis_mesh::util::tuning::init(agent_habilis_mesh::util::tuning::Tuning {
                ping_window_secs,
                longpoll_max_ms,
                directory_private,
                ..agent_habilis_mesh::util::tuning::Tuning::DEFAULTS
            });
            Box::pin(crate::mcp::run()).await
        }
        // The manual is embedded at compile time (`include_str!`), so the
        // binary documents itself with no repo checkout.
        Commands::Man => {
            print!("{}", include_str!("../../../../docs/manual.txt"));
            Ok(())
        }
        // Embedded integration artifacts written to the agents' skills dirs —
        // self-contained, no repo checkout needed (like `Man`).
        Commands::Plug { agents, paths } => plug::plug(&agents, &paths),
        Commands::Unplug { agents, paths } => plug::unplug(&agents, &paths),
        Commands::Doctor { opts } => doctor::run(opts).await,
    }
}

/// Build the output sink, set up the mesh, and run the event loop. The
/// shared spine of `create` and `join` — `resolved` carries the
/// already-resolved [`SetupKind`](agent_habilis_mesh::daemon::setup::SetupKind), author,
/// and advertise directory (see [`agent_habilis_mesh::daemon::params`]).
async fn run_session(resolved: Resolved, shared: SharedServerOpts) -> Result<()> {
    let Resolved {
        kind,
        author,
        advertise_directory,
    } = resolved;
    // The advertiser reaches the directory over this mesh's own lookups
    // (only `create` advertises, so only `Create` carries them).
    let directory_lookups = match &kind {
        SetupKind::Create { config, .. } => Some(config.lookups.clone()),
        SetupKind::Join { .. } | SetupKind::Topic { .. } => None,
    };
    let out = Output::new(OutputMode::Json, shared.filter_self);
    // Nag once at startup if an installed integration has fallen behind this
    // binary. CLI-only: the in-process paths pass `None` so in-process tests
    // stay hermetic. `agent-gossip status` is the on-demand counterpart.
    let drift = agent::home_dir()
        .ok()
        .and_then(|home| agent::drift_warning(&home));
    // The CLI owns the a2a application state. Tap its output so surfaced events
    // reach the app-side `poll`/`fetch` ring; the engine emits `NodeEvent`s
    // through `io.sink()` (`cfg.sink`) and the app's own `Output` render the
    // same tap.
    let io = crate::a2a::app::SurfacedIo::new(out);
    let sink = io.sink();
    let mut app = crate::a2a::app::A2aApp::with_io(io);
    // `--a2a-serve`: bind the localhost JSON-RPC binding here (the app owns it)
    // and hand the app the served request channel. The resolved (possibly
    // ephemeral-assigned) port is threaded back into setup for the `ready`
    // event + published card.
    let (a2a_serve_port, http_rx) = match shared.a2a_serve {
        Some(port) => {
            let binding = crate::a2a::http::bind(port).await?;
            let real_port = binding.port;
            let http_rx = app.serve_a2a(binding);
            (Some(real_port), Some(http_rx))
        }
        None => (None, None),
    };
    let mut cfg = setup_mesh(
        kind,
        SetupParams {
            author,
            max_peers: shared.max_peers,
            state_file: shared.state_file,
            sink,
            multihop: shared.tuning.multihop,
            drift: drift.as_deref(),
            a2a_serve: a2a_serve_port,
        },
    )
    .await?;
    // Advertising (`create --advertise`): start the re-broadcast task. It
    // reaches the directory over this mesh's own lookups. The handle is
    // held for the session's lifetime — on the CLI the process exits (via
    // signal) before it would drop, which tears the task down.
    let _advertiser = advertise_directory
        .zip(directory_lookups)
        .map(|(directory, lookups)| spawn_advertiser(&mut cfg, directory, lookups));
    // First point where mesh id + nickname are known — attach the
    // buffered log sink here (see `logging`).
    agent_habilis_mesh::logging::attach(&cfg.mesh, &cfg.author);
    run_event_loop(cfg, app, None, http_rx).await
}

/// Create a new mesh, print its identifier, and start listening.
async fn create(opts: CreateOpts) -> Result<()> {
    // Borrow-then-move: resolve the lookups and advertise selection (which
    // borrow `opts`) before moving `opts.name`/`opts.nickname` out.
    let advertise = opts.advertise_selection();
    let password = password::resolve_password(opts.password.clone())?;
    let config = MeshConfig {
        lookups: resolve_lookups(opts.public, opts.lookups.to_set()),
        // The verifier is baked in at setup: its salt is the seed, which is
        // minted there. The flag's presence is all `resolve` needs.
        password: None,
        // Likewise the issuer pubkey: `set_invite` mints the keypair at setup
        // and bakes the pubkey; the `--invite-only` flag rides `CreateParams`.
        issuer_pubkey: None,
        // `--no-gossip` is a mesh-wide characteristic baked into the id, so
        // every joiner inherits it from the ticket alone.
    };
    // `resolve` validates `--advertise` against the config (never a silent
    // no-op) before any setup work.
    let resolved = CreateParams {
        name: opts.name.unwrap_or_else(MeshName::random),
        nickname: opts.nickname,
        config,
        advertise,
        password,
        invite_only: opts.invite_only,
    }
    .resolve()?;
    // Boxed: the session future carries the full `EventLoopConfig` and sits
    // just over clippy's `large_futures` budget (same reason the event loop
    // itself is boxed).
    Box::pin(run_session(resolved, opts.shared)).await
}

/// Join an existing mesh by its identifier (💬...), a domain, or a
/// supported git repo URL. The mesh's config (lookups) is decoded from
/// the id — `join` takes no lookup flags.
async fn join(
    target: JoinTarget,
    nickname: Option<Nickname>,
    password_flag: Option<password::PasswordFlag>,
    shared: SharedServerOpts,
) -> Result<()> {
    let mut password = password::resolve_password(password_flag)?;
    // A protected id/invite with the flag absent is a hard error naming the
    // requirement (there is no interactive prompt). Both join-target variants
    // can be password-protected (a `💬` id or a `🎟️` invite), so classify
    // either before erroring.
    if password.is_none() {
        let requires = match &target {
            JoinTarget::Mesh(id) => id
                .as_str()
                .parse::<Mesh>()
                .is_ok_and(|mesh| mesh.requires_password())
                .then_some("room"),
            JoinTarget::Invite(invite) => invite.requires_password().then_some("invite"),
        };
        if let Some(what) = requires {
            password = Some(password::require_password(what)?);
        }
    }
    // `join` never advertises — that is a create-time decision.
    let resolved = JoinParams {
        target,
        nickname,
        password,
    }
    .resolve()?;
    Box::pin(run_session(resolved, shared)).await
}

/// Join a public mesh derived deterministically from a shared string. The
/// seed, name, and (always-public) config are all derived from the string, so
/// the same string joins the same topic on any machine — no id to share.
async fn topic(opts: TopicOpts) -> Result<()> {
    let resolved = TopicParams {
        string: opts.string,
        nickname: opts.nickname,
    }
    .resolve()?;
    Box::pin(run_session(resolved, opts.shared)).await
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

/// Post a message to a mesh via the running server's IPC socket.
#[expect(
    clippy::too_many_lines,
    reason = "flat dispatch over the a2a subcommand: the tunnel arms (expose/connect/discover) and the messaging arms (call/status/artifact) are each self-contained, and each carries a 6-8 field clap arg group, so splitting would only scatter the match into many-argument helpers"
)]
async fn a2a(action: A2aAction) -> Result<()> {
    match action {
        A2aAction::Expose {
            to,
            lookups,
            advertise,
            password,
            loopback,
            legacy_output: _,
        } => {
            let password = password::resolve_password(password)?;
            let advertise =
                agent_habilis_mesh::protocol::mesh::DirectorySelection::from_flag(advertise);
            Box::pin(crate::a2a::expose(crate::a2a::ExposeParams {
                to: &to,
                flags: lookups.to_set(),
                advertise,
                loopback,
                password,
            }))
            .await
        }
        A2aAction::Connect {
            ticket,
            port,
            password,
            legacy_output: _,
        } => {
            // Error when the ticket needs a password and none was passed
            // (mirrors the mesh-join UX); otherwise resolve the given flag.
            let password = match password {
                None if crate::a2a::ticket_requires_password(&ticket) => {
                    Some(password::require_password("ticket")?)
                }
                other => password::resolve_password(other)?,
            };
            crate::a2a::connect(&ticket, port, password).await
        }
        A2aAction::Discover {
            directory,
            lookups,
            legacy_output: _,
        } => {
            Box::pin(a2a_discover::discover(a2a_discover::DiscoverParams {
                directory,
                lookups: lookups.to_set(),
            }))
            .await
        }
        A2aAction::Call {
            room: mesh,
            nickname,
            to,
            method,
            text,
            task_id,
            params,
            timeout_secs,
        } => {
            // No peer + SendMessage = mesh broadcast chat (fire-and-forget).
            if to.is_none() && method == "SendMessage" {
                let text = text.ok_or_else(|| {
                    anyhow::anyhow!("broadcast SendMessage needs --text (or a --to peer)")
                })?;
                let body = agent_habilis_mesh::protocol::MessageBody::new(text)
                    .map_err(|error| anyhow::anyhow!("{error}"))?;
                let resp = ipc::send(&IpcCommand::Msg { mesh, body }, &nickname).await?;
                let id = finish_send(&resp, "message")?;
                println!("{id}");
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
                None => compose_a2a_params(&method, &mesh, text.as_deref(), task_id.as_ref()),
            };
            let cmd = IpcCommand::A2aCall {
                mesh,
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
            room: mesh,
            nickname,
            task_id,
            state,
            note,
        } => {
            let cmd = IpcCommand::A2aStatus {
                mesh,
                task_id,
                state,
                note,
            };
            let resp = ipc::send(&cmd, &nickname).await?;
            let id = finish_send(&resp, "task status")?;
            println!("{id}");
            Ok(())
        }
        A2aAction::Artifact {
            room: mesh,
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
                mesh,
                task_id,
                text: text.unwrap_or_default(),
                file,
                file_name,
                file_mime,
            };
            let resp = ipc::send(&cmd, &nickname).await?;
            let id = finish_send(&resp, "task artifact")?;
            println!("{id}");
            Ok(())
        }
        A2aAction::Fetch {
            ticket,
            nickname,
            output,
            password,
        } => {
            let ticket = agent_habilis_mesh::blob::BlobTicket::decode(&ticket)?;
            let password = password.map(agent_habilis_mesh::protocol::crypto::Password::new);

            // Where the bytes land, `None` meaning stdout. Precedence:
            //   `--output -`       → stdout
            //   `--output <path>`  → that path
            //   `--nickname <n>`   → `<session-dir>/<n>.recv/<sha256hex>`
            //   (none)             → stdout (session-less pipe, unchanged)
            let dest: Option<std::path::PathBuf> = match output {
                Some(path) if path.as_os_str() == "-" => None,
                Some(path) => Some(path),
                None => match &nickname {
                    Some(nick) => {
                        let mesh = session::mesh_for_nickname(nick.as_str()).ok_or_else(|| {
                            anyhow::anyhow!(
                                "no live session for nickname '{}' — start one, or pass --output",
                                nick.as_str()
                            )
                        })?;
                        // Validate the private base (fail closed) before the
                        // receive dir is created under it.
                        let dir = agent_habilis_mesh::util::ensure_mesh_runtime_dir(&mesh)
                            .map_err(|error| {
                                anyhow::anyhow!("cannot prepare receive dir: {error}")
                            })?
                            .join(format!("{}.recv", nick.as_str()));
                        tokio::fs::create_dir_all(&dir).await?;
                        Some(dir.join(ticket.sha256_hex()))
                    }
                    None => None,
                },
            };

            match dest {
                None => {
                    let mut stdout = tokio::io::stdout();
                    agent_habilis_mesh::blob::fetch(&ticket, &mut stdout, password).await?;
                }
                Some(path) => {
                    let mut file = tokio::fs::File::create(&path).await?;
                    if let Err(error) =
                        agent_habilis_mesh::blob::fetch(&ticket, &mut file, password).await
                    {
                        // fetch verifies the hash as it streams; a partial file
                        // from a failed transfer is meaningless, so drop it.
                        drop(file);
                        let _ = tokio::fs::remove_file(&path).await;
                        return Err(error);
                    }
                    println!("{}", path.display());
                }
            }
            Ok(())
        }
    }
}

fn compose_a2a_params(
    method: &str,
    mesh: &MeshId,
    text: Option<&str>,
    task_id: Option<&crate::a2a::TaskId>,
) -> serde_json::Value {
    match method {
        "SendMessage" | "SendStreamingMessage" => {
            let message =
                crate::a2a::gossip::send_message_payload(mesh, task_id, text.unwrap_or_default());
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
        room,
        nickname,
        state_file,
        after,
        long,
        legacy_output: _,
    } = opts;
    // clap enforces exactly one form: `--state-file`, or `--room` +
    // `--nickname`. The state-file form waits for readiness first so a poll
    // can be armed before the daemon has minted its identity.
    let (mesh, nickname) = if let Some(path) = state_file {
        wait_for_ready(&path, agent_habilis_mesh::util::tuning::READY_MAX_SECS).await?;
        let identity = agent_habilis_mesh::daemon::state_file::read_identity(&path);
        let missing = |field: &'static str| {
            anyhow::anyhow!("state file {} carries no {field}", path.display())
        };
        let mesh: MeshId = identity.mesh.ok_or_else(|| missing("room id"))?.parse()?;
        let nick: Nickname = identity
            .nickname
            .ok_or_else(|| missing("nickname"))?
            .parse()?;
        (mesh, nick)
    } else {
        (
            room.expect("clap: --room required without --state-file"),
            nickname.expect("clap: --nickname required without --state-file"),
        )
    };
    let cmd = IpcCommand::Poll { mesh, after, long };

    loop {
        let started = tokio::time::Instant::now();
        let resp = ipc::send(&cmd, &nickname).await?;
        // An empty response is the daemon closing the connection without
        // writing (shutdown mid-park) — `round_trip` maps that EOF to `""`.
        // Surface it instead of printing a blank line / retrying forever.
        anyhow::ensure!(
            !resp.is_empty(),
            "room daemon closed the connection without a response (shutting down?)"
        );
        // Clean daemon shutdown: end the poll with the truthful empty batch
        // and exit 0 — re-issuing would hit a socket that is about to vanish.
        // The sentinel is a control response and never reaches stdout. A
        // daemon that dies WITHOUT sending it (crash, SIGKILL) still errors.
        if resp == crate::a2a::surfaced::SHUTDOWN_SENTINEL {
            println!("[]");
            return Ok(());
        }
        if !(long && resp == "[]") {
            println!("{resp}");
            return Ok(());
        }
        // Both empty paths render exactly `[]`; an error response is a JSON
        // object, so it prints and exits above. Floor the cycle so a daemon
        // degrading long reads to immediate empties (waiter registry full)
        // can't spin this loop hot — the unchanged cursor re-reads anything
        // that lands during the sleep.
        let min_cycle = std::time::Duration::from_millis(
            agent_habilis_mesh::util::tuning::POLL_LONG_MIN_CYCLE_MS,
        );
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
        room: mesh,
        nickname,
        legacy_output: _,
    } = opts;
    let cmd = IpcCommand::Ping { mesh };
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
        room: mesh,
        nickname,
        legacy_output: _,
    } = opts;
    let cmd = IpcCommand::Peers { mesh };
    let resp = ipc::send(&cmd, &nickname).await?;
    println!("{resp}");
    Ok(())
}

/// Parse a `--ttl` value into seconds: a bare number (seconds), a `s`/`m`/`h`/`d`
/// suffixed duration, or `none`/`0` for no expiry. Absent ⇒ the 24h default.
fn parse_ttl(raw: Option<&str>) -> Result<u64> {
    let Some(raw) = raw.map(str::trim) else {
        return Ok(agent_habilis_mesh::util::consts::INVITE_DEFAULT_TTL_SECS);
    };
    if raw.eq_ignore_ascii_case("none") {
        return Ok(0);
    }
    let (digits, multiplier) = match raw.as_bytes().last() {
        Some(b's' | b'S') => (&raw[..raw.len() - 1], 1),
        Some(b'm' | b'M') => (&raw[..raw.len() - 1], 60),
        Some(b'h' | b'H') => (&raw[..raw.len() - 1], 3600),
        Some(b'd' | b'D') => (&raw[..raw.len() - 1], 86_400),
        _ => (raw, 1),
    };
    let value: u64 = digits.trim().parse().map_err(|_| {
        anyhow::anyhow!("invalid --ttl `{raw}` — use seconds, `1h`, `30m`, `7d`, or `none`")
    })?;
    Ok(value.saturating_mul(multiplier))
}

/// Mint a 🎟️ invite via the creating session's daemon (only it holds the issuer
/// key). Human mode prints just the copyable token; `--output json` prints the
/// raw `{ok,invite}` line. Exits non-zero on refusal (e.g. not the creator).
async fn invite(opts: InviteOpts) -> Result<()> {
    let InviteOpts {
        room: mesh,
        nickname,
        ttl,
        legacy_output: _,
    } = opts;
    let ttl = parse_ttl(ttl.as_deref())?;
    let cmd = IpcCommand::Invite { mesh, ttl };
    let resp = ipc::send(&cmd, &nickname).await?;
    println!("{resp}");
    let parsed: serde_json::Value = serde_json::from_str(&resp)?;
    if parsed.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        std::process::exit(1);
    }
    Ok(())
}

/// Print the multihop routing topology (assembled mesh graph) from the running
/// daemon, as JSON. Backs the `/room:topology` render.
async fn topology_cmd(opts: TopologyOpts) -> Result<()> {
    let cmd = IpcCommand::Topology { mesh: opts.room };
    let resp = ipc::send(&cmd, &opts.nickname).await?;
    println!("{resp}");
    let parsed: MsgResponse = serde_json::from_str(&resp)?;
    if !parsed.ok {
        std::process::exit(1);
    }
    Ok(())
}

/// Read or change the mesh's shared state via the running daemon. Emits the
/// raw IPC JSON response — `{ok,...}` for `merge`, `{ok,document}` for `get`.
async fn state(opts: StateOpts) -> Result<()> {
    let (cmd, nickname) = match opts.action {
        StateAction::Merge {
            room: mesh,
            nickname,
            merge,
        } => {
            let merge_doc: serde_json::Value = serde_json::from_str(&merge).map_err(|error| {
                anyhow::anyhow!("--merge must be valid JSON (an RFC 7386 merge patch): {error}")
            })?;
            (
                IpcCommand::StateMerge {
                    mesh,
                    merge: merge_doc,
                },
                nickname,
            )
        }
        StateAction::Get {
            room: mesh,
            nickname,
        } => (IpcCommand::StateGet { mesh }, nickname),
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

/// Read or change the mesh's `meta` channel via the running daemon — the
/// independent counterpart of [`state`]. Same raw-JSON + exit-code contract.
async fn meta(opts: MetaOpts) -> Result<()> {
    let (cmd, nickname) = match opts.action {
        MetaAction::Merge {
            room: mesh,
            nickname,
            merge,
        } => {
            let merge_doc: serde_json::Value = serde_json::from_str(&merge).map_err(|error| {
                anyhow::anyhow!("--merge must be valid JSON (an RFC 7386 merge patch): {error}")
            })?;
            (
                IpcCommand::MetaMerge {
                    mesh,
                    merge: merge_doc,
                },
                nickname,
            )
        }
        MetaAction::Get {
            room: mesh,
            nickname,
        } => (IpcCommand::MetaGet { mesh }, nickname),
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
/// Prints `{mesh,name,nickname}` on success, so the gate doubles as the
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
        legacy_output: _,
    } = opts;
    wait_for_ready(&state_file, timeout_secs).await?;
    print_ready_identity(&state_file);
    Ok(())
}

/// Block until `state_file` reports a fresh `ready: true`, or fail at the
/// deadline. The wait behind both the `ready` gate and `poll --state-file`.
async fn wait_for_ready(state_file: &std::path::Path, timeout_secs: u64) -> Result<()> {
    // Saturating add: an absurd `--timeout-secs` must not panic the gate
    // (`Instant + Duration` panics on overflow); clamp to a far-future
    // deadline instead.
    let now = tokio::time::Instant::now();
    let deadline = now
        .checked_add(std::time::Duration::from_secs(timeout_secs))
        .unwrap_or_else(|| {
            now + std::time::Duration::from_secs(agent_habilis_mesh::util::tuning::READY_MAX_SECS)
        });
    loop {
        // Read off the runtime's blocking pool: a `--state-file` on a hung
        // mount must not block a worker thread. A read error (malformed/torn/
        // old-schema file) is treated as not-ready, not a hard failure — the
        // daemon may be mid-overwrite. A *persistent* error (e.g. a bad path)
        // can't self-heal and just spins to the deadline, so log it so the
        // cause is recoverable.
        let path = state_file.to_path_buf();
        let read = tokio::task::spawn_blocking(move || {
            agent_habilis_mesh::daemon::state_file::read_snapshot(&path)
        })
        .await?;
        match read {
            Ok(Some(snapshot)) if snapshot.ready && ready_is_fresh(snapshot.last_updated) => {
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
            agent_habilis_mesh::util::tuning::READY_POLL_INTERVAL_MS,
        ))
        .await;
    }
}

/// Print the session identity as a JSON object for `agent-gossip ready --output json`,
/// omitting any field the state file lacks — so a degenerate (identity-less)
/// file yields `{}` rather than `{"room":null,…}` that a caller might splice
/// into the next command as the literal string "null".
fn print_ready_identity(state_file: &std::path::Path) {
    let identity = agent_habilis_mesh::daemon::state_file::read_identity(state_file);
    let mut obj = serde_json::Map::new();
    for (key, value) in [
        ("room", identity.mesh),
        ("name", identity.name),
        ("nickname", identity.nickname),
        ("topic", identity.topic),
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
    let now = agent_habilis_mesh::util::clock::unix_secs();
    let last_updated = i64::try_from(last_updated).unwrap_or(i64::MAX);
    let skew = now - last_updated; // >0: file is in the past; <0: in the future
    let window =
        i64::try_from(agent_habilis_mesh::util::tuning::READY_FRESH_SECS).unwrap_or(i64::MAX);
    skew.abs() <= window
}
