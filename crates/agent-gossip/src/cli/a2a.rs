use anyhow::Result;

use super::a2a_call_request::A2aCallRequest;
use super::a2a_call_sugar::A2aCallSugar;
use super::advertised_file_name::advertised_file_name;
use super::args::A2aAction;
use super::finish_send::finish_send;
use crate::a2a::ipc_command::IpcCommand;

/// Post a message to a mesh via the running server's IPC socket.
#[expect(
    clippy::too_many_lines,
    reason = "flat dispatch over the a2a subcommand: the tunnel arms (expose/connect/discover) and the messaging arms (call/status/artifact) are each self-contained, and each carries a 6-8 field clap arg group, so splitting would only scatter the match into many-argument helpers"
)]
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
        A2aAction::Connect {
            ticket,
            port,
            password,
            allow_anonymous,
            legacy_output: _,
        } => {
            // Error when the ticket needs a password and none was passed
            // (mirrors the mesh-join UX); otherwise resolve the given flag.
            let password = match password {
                None if crate::a2a::ticket_requires_password(&ticket) => {
                    Some(super::password_flag::require_password("ticket")?)
                }
                other => super::password_flag::resolve_password(other)?,
            };
            crate::bridge::bridge::connect(&ticket, port, password, allow_anonymous).await
        }
        A2aAction::Discover {
            directory,
            lookups,
            legacy_output: _,
        } => {
            Box::pin(super::discover_params::discover(
                super::discover_params::DiscoverParams {
                    directory,
                    lookups: lookups.to_set()?,
                },
            ))
            .await
        }
        A2aAction::Broadcast {
            gossip: mesh,
            nickname,
            text,
        } => {
            let body = habilis_network::protocol::MessageBody::new(text)
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            let response =
                super::send::send(&IpcCommand::Broadcast { mesh, body }, &nickname).await?;
            let id = finish_send(&response, "message")?;
            println!("{id}");
            Ok(())
        }
        A2aAction::Msg {
            gossip: mesh,
            nickname,
            to,
            text,
        } => {
            let body = habilis_network::protocol::MessageBody::new(text)
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            let response =
                super::send::send(&IpcCommand::Msg { mesh, to, body }, &nickname).await?;
            let id = finish_send(&response, "message")?;
            println!("{id}");
            Ok(())
        }
        A2aAction::Call {
            gossip: mesh,
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
        A2aAction::Status {
            gossip: mesh,
            nickname,
            task_id,
            state,
            note,
            waiting,
        } => {
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
        A2aAction::Artifact {
            gossip: mesh,
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
        A2aAction::Fetch {
            ticket,
            nickname,
            output,
            password,
        } => {
            let ticket = habilis_network::ops::blob::BlobTicket::decode(&ticket)?;
            let password = password.map(habilis_network::protocol::Password::new);

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
                        let mesh = super::session::mesh_for_nickname(nick.as_str()).ok_or_else(|| {
                            anyhow::anyhow!(
                                "no live session for nickname '{}' — start one, or pass --output",
                                nick.as_str()
                            )
                        })?;
                        // Validate the private base (fail closed) before the
                        // receive dir is created under it.
                        let dir = crate::ensure_mesh_runtime_dir(&mesh)
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
                    habilis_network::ops::blob::fetch(&ticket, &mut stdout, password).await?;
                }
                Some(path) => {
                    let mut file = tokio::fs::File::create(&path).await?;
                    if let Err(error) =
                        habilis_network::ops::blob::fetch(&ticket, &mut file, password).await
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
