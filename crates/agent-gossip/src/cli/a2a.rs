use anyhow::Result;
use habilis_network::ops::blob;
use habilis_network::ops::blob::BlobTicket;
use habilis_network::protocol::{MeshId, MeshName, Nickname};

use super::a2a_call_request::A2aCallRequest;
use super::a2a_call_sugar::A2aCallSugar;
use super::advertised_file_name::advertised_file_name;
use super::args::A2aAction;
use super::args::lookup::LookupArgs;
use super::finish_send::finish_send;
use super::password_flag::PasswordFlag;
use crate::a2a::ipc_command::IpcCommand;
use crate::a2a::{TaskId, TaskState};
use crate::bridge::bridge;

/// Post a message to a mesh via the running server's IPC socket.
pub(super) async fn a2a(action: A2aAction) -> Result<()> {
    match action {
        A2aAction::Expose {
            to,
            lookups,
            advertise,
            password,
            loopback,
            legacy_output: _,
        } => {
            expose(ExposeArgs {
                to,
                lookups,
                advertise,
                password,
                loopback,
            })
            .await
        }
        A2aAction::Connect {
            ticket,
            port,
            password,
            allow_anonymous,
            legacy_output: _,
        } => connect(&ticket, port, password, allow_anonymous).await,
        A2aAction::Discover {
            directory,
            lookups,
            legacy_output: _,
        } => discover(directory, lookups).await,
        A2aAction::Broadcast {
            gossip,
            nickname,
            text,
        } => broadcast(gossip, &nickname, text).await,
        A2aAction::Msg {
            gossip,
            nickname,
            to,
            text,
        } => send_msg(gossip, &nickname, to, text).await,
        A2aAction::Call {
            gossip,
            nickname,
            to,
            method,
            text,
            task_id,
            label,
            params,
            timeout_secs,
            file,
            file_name,
            file_mime,
        } => {
            call(CallArgs {
                mesh: gossip,
                nickname,
                to,
                method,
                text,
                task_id,
                label,
                params,
                timeout_secs,
                file,
                file_name,
                file_mime,
            })
            .await
        }
        A2aAction::Status {
            gossip,
            nickname,
            task_id,
            state,
            note,
            waiting,
        } => {
            status(StatusArgs {
                mesh: gossip,
                nickname,
                task_id,
                state,
                note,
                waiting,
            })
            .await
        }
        A2aAction::Artifact {
            gossip,
            nickname,
            task_id,
            text,
            file,
            file_name,
            file_mime,
        } => {
            artifact(ArtifactArgs {
                mesh: gossip,
                nickname,
                task_id,
                text,
                file,
                file_name,
                file_mime,
            })
            .await
        }
        A2aAction::Fetch {
            ticket,
            nickname,
            output,
            password,
        } => fetch(&ticket, nickname.as_ref(), output, password).await,
    }
}

/// The fields of an `a2a expose`, grouped so the helper stays within the argument limit.
struct ExposeArgs {
    to: String,
    lookups: LookupArgs,
    advertise: Option<MeshName>,
    password: Option<PasswordFlag>,
    loopback: bool,
}

async fn expose(args: ExposeArgs) -> Result<()> {
    let ExposeArgs {
        to,
        lookups,
        advertise,
        password,
        loopback,
    } = args;
    let password = super::password_flag::resolve_password(password)?;
    let advertise = habilis_network::protocol::DirectorySelection::from_flag(advertise);
    Box::pin(crate::a2a::expose(crate::a2a::ExposeParams {
        to: &to,
        flags: lookups.to_set()?,
        advertise,
        loopback,
        password,
    }))
    .await
}

async fn connect(
    ticket: &str,
    port: Option<u16>,
    password: Option<PasswordFlag>,
    allow_anonymous: bool,
) -> Result<()> {
    // Error when the ticket needs a password and none was passed
    // (mirrors the mesh-join UX); otherwise resolve the given flag.
    let password = match password {
        None if crate::a2a::ticket_requires_password(ticket) => {
            Some(super::password_flag::require_password("ticket")?)
        }
        other => super::password_flag::resolve_password(other)?,
    };
    bridge::connect(ticket, port, password, allow_anonymous).await
}

async fn discover(directory: Option<MeshName>, lookups: LookupArgs) -> Result<()> {
    Box::pin(super::discover_params::discover(
        super::discover_params::DiscoverParams {
            directory,
            lookups: lookups.to_set()?,
        },
    ))
    .await
}

async fn broadcast(mesh: MeshId, nickname: &Nickname, text: String) -> Result<()> {
    let body = habilis_network::protocol::MessageBody::try_new(text)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let response = super::send::send(&IpcCommand::Broadcast { mesh, body }, nickname).await?;
    let id = finish_send(&response, "message")?;
    println!("{id}");
    Ok(())
}

async fn send_msg(mesh: MeshId, nickname: &Nickname, to: Nickname, text: String) -> Result<()> {
    let body = habilis_network::protocol::MessageBody::try_new(text)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let response = super::send::send(&IpcCommand::Msg { mesh, to, body }, nickname).await?;
    let id = finish_send(&response, "message")?;
    println!("{id}");
    Ok(())
}

/// The fields of an `a2a call`, grouped so the helper stays within the argument limit.
struct CallArgs {
    mesh: MeshId,
    nickname: Nickname,
    to: Nickname,
    method: String,
    text: Option<String>,
    task_id: Option<TaskId>,
    label: Option<String>,
    params: Option<String>,
    timeout_secs: u64,
    file: Option<std::path::PathBuf>,
    file_name: Option<String>,
    file_mime: Option<String>,
}

async fn call(args: CallArgs) -> Result<()> {
    let CallArgs {
        mesh,
        nickname,
        to,
        method,
        text,
        task_id,
        label,
        params,
        timeout_secs,
        file,
        file_name,
        file_mime,
    } = args;
    // Compose params from --text/--task-id/--label sugar, unless raw
    // --params given.
    let params: serde_json::Value = match params {
        Some(raw) => serde_json::from_str(&raw)
            .map_err(|error| anyhow::anyhow!("--params must be valid JSON: {error}"))?,
        None => A2aCallRequest::parse(
            &method,
            &A2aCallSugar {
                mesh: &mesh,
                text: text.as_deref(),
                task_id: task_id.as_ref(),
                label: label.as_deref(),
            },
        )?
        .params(),
    };
    // The daemon runs in its own directory, so a relative path must
    // resolve here, against the caller's.
    let file = file.map(std::path::absolute).transpose()?;
    let file_name = advertised_file_name(file.as_deref(), file_name);
    let command = IpcCommand::A2aCall {
        mesh,
        to,
        method,
        params,
        timeout_secs,
        file,
        file_name,
        file_mime,
    };
    let response = super::send::send(&command, &nickname).await?;
    println!("{response}");
    let parsed: serde_json::Value = serde_json::from_str(&response)?;
    if !parsed["error"].is_null() {
        std::process::exit(1);
    }
    Ok(())
}

/// The fields of an `a2a status`, grouped so the helper stays within the argument limit.
struct StatusArgs {
    mesh: MeshId,
    nickname: Nickname,
    task_id: TaskId,
    state: TaskState,
    note: Option<String>,
    waiting: Option<String>,
}

async fn status(args: StatusArgs) -> Result<()> {
    let StatusArgs {
        mesh,
        nickname,
        task_id,
        state,
        note,
        waiting,
    } = args;
    crate::a2a::broadcast_params::WorkerLeg::parse(state, waiting.as_deref())
        .map_err(|_| anyhow::anyhow!("--waiting requires --state working"))?;
    let command = IpcCommand::A2aStatus {
        mesh,
        task_id,
        state,
        note,
        waiting,
    };
    let response = super::send::send(&command, &nickname).await?;
    let id = finish_send(&response, "task status")?;
    println!("{id}");
    Ok(())
}

/// The fields of an `a2a artifact`, grouped so the helper stays within the argument limit.
struct ArtifactArgs {
    mesh: MeshId,
    nickname: Nickname,
    task_id: TaskId,
    text: Option<String>,
    file: Option<std::path::PathBuf>,
    file_name: Option<String>,
    file_mime: Option<String>,
}

async fn artifact(args: ArtifactArgs) -> Result<()> {
    let ArtifactArgs {
        mesh,
        nickname,
        task_id,
        text,
        file,
        file_name,
        file_mime,
    } = args;
    if text.is_none() && file.is_none() {
        anyhow::bail!("provide --text and/or --file");
    }
    let file = file.map(std::path::absolute).transpose()?;
    let file_name = advertised_file_name(file.as_deref(), file_name);
    let command = IpcCommand::A2aArtifact {
        mesh,
        task_id,
        text: text.unwrap_or_default(),
        file,
        file_name,
        file_mime,
    };
    let response = super::send::send(&command, &nickname).await?;
    let id = finish_send(&response, "task artifact")?;
    println!("{id}");
    Ok(())
}

async fn fetch(
    ticket: &str,
    nickname: Option<&Nickname>,
    output: Option<std::path::PathBuf>,
    password: Option<String>,
) -> Result<()> {
    let ticket = BlobTicket::decode(ticket)?;
    let password = password.map(habilis_network::protocol::Password::new);

    // Where the bytes land, `None` meaning stdout. Precedence:
    //   `--output -`       → stdout
    //   `--output <path>`  → that path
    //   `--nickname <n>`   → `<session-dir>/<n>.recv/<sha256hex>`
    //   (none)             → stdout (session-less pipe, unchanged)
    let dest: Option<std::path::PathBuf> = match output {
        Some(path) if path.as_os_str() == "-" => None,
        Some(path) => Some(path),
        None => match nickname {
            Some(nick) => {
                let mesh = super::session::mesh_for_nickname(nick.as_str()).ok_or_else(|| {
                    anyhow::anyhow!(
                        "no live session for nickname '{}' — start one, or pass --output",
                        nick.as_str()
                    )
                })?;
                // Validate the private base (fail closed) before the
                // receive dir is created under it.
                let dir = crate::ensure_mesh_runtime_dir(&mesh)
                    .map_err(|error| anyhow::anyhow!("cannot prepare receive dir: {error}"))?
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
            blob::fetch(&ticket, &mut stdout, password).await?;
        }
        Some(path) => {
            let mut file = tokio::fs::File::create(&path).await?;
            if let Err(error) = blob::fetch(&ticket, &mut file, password).await {
                // fetch verifies the hash as it streams; a partial file
                // from a failed transfer is meaningless, so drop it.
                drop(file);
                let _ = tokio::fs::remove_file(&path).await;
                return Err(error.into());
            }
            println!("{}", path.display());
        }
    }
    Ok(())
}
