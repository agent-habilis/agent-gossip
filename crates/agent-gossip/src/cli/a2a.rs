use anyhow::Result;
use habilis_network::ops::blob;
use habilis_network::ops::blob::BlobTicket;
use habilis_network::protocol::{MessageBody, Nickname};

use super::a2a_call_request::A2aCallRequest;
use super::a2a_call_sugar::A2aCallSugar;
use super::a2a_discover::a2a_discover;
use super::advertised_file_name::advertised_file_name;
use super::args::{A2aAction, ArtifactArgs, CallArgs, ExposeArgs, SessionAddress, StatusArgs};
use super::finish_send::finish_send;
use super::password_flag::PasswordFlag;
use crate::a2a::WorkerLeg;
use crate::a2a::ipc_command::IpcCommand;
use crate::tunnel::bridge;

pub(super) async fn a2a(action: A2aAction) -> Result<()> {
    match action {
        A2aAction::Expose(args) => expose(args).await,
        A2aAction::Connect {
            ticket,
            port,
            password,
            allow_anonymous,
        } => connect(&ticket, port, password, allow_anonymous).await,
        A2aAction::Discover { directory, lookups } => {
            Box::pin(a2a_discover(directory, lookups.to_set()?)).await
        }
        A2aAction::Broadcast { session, text } => {
            let command = IpcCommand::Broadcast {
                mesh: session.gossip,
                body: message_body(&text)?,
            };
            send_and_print_id(&command, &session.nickname, "message").await
        }
        A2aAction::Msg { session, to, text } => {
            let command = IpcCommand::Msg {
                mesh: session.gossip,
                to,
                body: message_body(&text)?,
            };
            send_and_print_id(&command, &session.nickname, "message").await
        }
        A2aAction::Call(args) => call(args).await,
        A2aAction::Status(args) => status(args).await,
        A2aAction::Artifact(args) => artifact(args).await,
        A2aAction::Fetch {
            ticket,
            nickname,
            output,
            password,
        } => fetch(&ticket, nickname.as_ref(), output, password).await,
    }
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
    let advertise = habilis_network::protocol::DirectorySelection::from(advertise);
    Box::pin(crate::tunnel::expose(crate::tunnel::ExposeParams {
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
    let password = super::password_flag::resolve_password(password)?;
    bridge::connect(ticket, port, password, allow_anonymous).await
}

fn message_body(text: &str) -> Result<MessageBody> {
    MessageBody::try_new(text).map_err(|error| anyhow::anyhow!("{error}"))
}

async fn send_and_print_id(command: &IpcCommand, nickname: &Nickname, what: &str) -> Result<()> {
    let response = super::send::send(command, nickname).await?;
    let id = finish_send(&response, what)?;
    println!("{id}");
    Ok(())
}

async fn call(args: CallArgs) -> Result<()> {
    let CallArgs {
        session: SessionAddress {
            gossip: mesh,
            nickname,
        },
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

async fn status(args: StatusArgs) -> Result<()> {
    let StatusArgs {
        session: SessionAddress {
            gossip: mesh,
            nickname,
        },
        task_id,
        state,
        text,
        progress,
        waiting_on_user,
    } = args;
    let leg = WorkerLeg::parse(state, text, progress, waiting_on_user)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let command = IpcCommand::A2aStatus { mesh, task_id, leg };
    send_and_print_id(&command, &nickname, "task status").await
}

async fn artifact(args: ArtifactArgs) -> Result<()> {
    let ArtifactArgs {
        session: SessionAddress {
            gossip: mesh,
            nickname,
        },
        task_id,
        text,
        file,
        file_name,
        file_mime,
    } = args;
    let blank_text = text.as_deref().is_none_or(|text| text.trim().is_empty());
    if blank_text && file.is_none() {
        anyhow::bail!("an artifact needs a text or a file");
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
    send_and_print_id(&command, &nickname, "task artifact").await
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
