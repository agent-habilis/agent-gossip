use fofoca::ops::MeshSender;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use std::collections::HashMap;
use std::time::Duration;

use crate::a2a::app::A2aApp;
use crate::a2a::surfaced::{PollOrRegisterParams, PollResponder};
use crate::a2a::{TaskId, TaskState};
use crate::output;
use fofoca::embed::{EventLoopState, PingRound};
use fofoca::protocol::MeshName;
use fofoca::protocol::{MeshId, Message, MessageBody, Nickname};
use fofoca::runtime::ipc::{Addressed, json_ack, json_error, json_ok_msg};
use fofoca::runtime::tuning::ping_window_secs;

use crate::a2a::send::{
    BroadcastParams, MsgParams, TaskArtifactEmitParams, TaskStatusParams, emit_task_artifact,
    emit_task_status, send_broadcast, send_msg,
};
use fofoca::ops::{StateMergeParams, broadcast_msg, broadcast_state_merge};

/// Command sent from CLI to the running server over IPC. App-side because its
/// arms carry a2a-typed payloads (task id/state, gossip A2A calls); the engine's
/// `transport::ipc` socket server is generic over this type and names none of it.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command")]
pub(crate) enum IpcCommand {
    /// Broadcast a mesh chat message (A2A `message/send` with no addressee).
    #[serde(rename = "broadcast")]
    Broadcast { mesh: MeshId, body: MessageBody },
    /// Send a chat message to one peer (A2A `message/send` addressed to it).
    /// Distinct from `a2a_call` with `SendMessage`, which opens a *task*; this
    /// is a chat line that happens to be private.
    #[serde(rename = "msg")]
    Msg {
        mesh: MeshId,
        to: Nickname,
        body: MessageBody,
    },
    #[serde(rename = "poll")]
    Poll {
        mesh: MeshId,
        /// Surfaced-event seq cursor: return events surfaced after this seq.
        /// Omitted on the first poll (returns the buffered history). The
        /// per-event `seq` in the response is the value to pass next.
        #[serde(skip_serializing_if = "Option::is_none")]
        after: Option<u64>,
        /// Long-poll: park this read up to the server cap (`longpoll_max_ms`),
        /// returning early on the first new surfaced event, else `[]` at the
        /// deadline. Absent/false is an immediate read. Skipped when false so
        /// the wire stays byte-stable for callers that never long-poll.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        long: bool,
    },
    /// Arm an RTT round: the daemon broadcasts a ping probe, collects
    /// pongs for a fixed window, and emits a `ping_report` on its
    /// `--output json` stream. Fire-and-forget — the ack is immediate.
    #[serde(rename = "ping")]
    Ping { mesh: MeshId },
    /// Worker-emit a `TaskStatusUpdate` on a task we're serving (`a2a status`).
    #[serde(rename = "a2a_status")]
    A2aStatus {
        mesh: MeshId,
        task_id: TaskId,
        #[serde(with = "crate::a2a::friendly_state")]
        state: TaskState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// Worker-emit a `TaskArtifactUpdate` (the result) on a task (`a2a artifact`).
    /// An optional `file` is offloaded over the blob channel and referenced as a
    /// `Part.url`; a path (not bytes) so the IPC line stays bounded.
    #[serde(rename = "a2a_artifact")]
    A2aArtifact {
        mesh: MeshId,
        task_id: TaskId,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<std::path::PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_mime: Option<String>,
    },
    /// Query the live peer roster (nicknames + recency) — backs the
    /// task sender's target picker and nickname validation.
    #[serde(rename = "peers")]
    Peers { mesh: MeshId },
    /// Apply an RFC 7386 JSON Merge Patch to the mesh's shared state. `merge` is
    /// any JSON value merged into the document (an object deep-merges, `null`
    /// deletes a key, a non-object replaces the target); the daemon signs +
    /// gossips it.
    #[serde(rename = "state_merge")]
    StateMerge {
        mesh: MeshId,
        merge: serde_json::Value,
    },
    /// Read the current derived shared-state document.
    #[serde(rename = "state_get")]
    StateGet { mesh: MeshId },
    /// `meta`-channel counterpart of [`StateMerge`](IpcCommand::StateMerge).
    #[serde(rename = "meta_merge")]
    MetaMerge {
        mesh: MeshId,
        merge: serde_json::Value,
    },
    /// `meta`-channel counterpart of [`StateGet`](IpcCommand::StateGet).
    #[serde(rename = "meta_get")]
    MetaGet { mesh: MeshId },
    /// The relay routing topology from this daemon's point of view: the
    /// metric-weighted mesh graph it has assembled from gossiped link-state.
    #[serde(rename = "topology")]
    Topology { mesh: MeshId },
    /// A gossip A2A call: send a directed A2A JSON-RPC request to `to` and
    /// return its response (or a timeout error). `to` serves the safe method
    /// set only.
    #[serde(rename = "a2a_call")]
    A2aCall {
        mesh: MeshId,
        to: Nickname,
        method: String,
        #[serde(default)]
        params: serde_json::Value,
        timeout_secs: u64,
    },
    /// Identity probe for `doctor`: the daemon answers with its own mesh id,
    /// human name, nickname, and peer count. Carries no mesh — a
    /// socket serves exactly one mesh and `doctor` is asking *which*, so it
    /// addresses the daemon by socket path, not by id.
    #[serde(rename = "info")]
    Info,
    /// Mint an invite for the mesh this socket serves — creator-only
    /// (the daemon holds the issuer key in `state.mint_mesh()`). `ttl` is the
    /// invite lifetime in seconds (`0` ⇒ no expiry).
    #[serde(rename = "invite")]
    Invite { mesh: MeshId, ttl: u64 },
}

impl Addressed for IpcCommand {
    /// The mesh a command is addressed to, used to derive the socket path in
    /// [`fofoca::runtime::ipc::send`]. `None` for [`IpcCommand::Info`], which is
    /// sent by socket path directly (the daemon answers with its own id).
    fn mesh_id(&self) -> Option<&MeshId> {
        match self {
            IpcCommand::Broadcast { mesh, .. }
            | IpcCommand::Msg { mesh, .. }
            | IpcCommand::Poll { mesh, .. }
            | IpcCommand::Ping { mesh }
            | IpcCommand::A2aStatus { mesh, .. }
            | IpcCommand::A2aArtifact { mesh, .. }
            | IpcCommand::Peers { mesh }
            | IpcCommand::StateMerge { mesh, .. }
            | IpcCommand::StateGet { mesh }
            | IpcCommand::MetaMerge { mesh, .. }
            | IpcCommand::MetaGet { mesh }
            | IpcCommand::Topology { mesh }
            | IpcCommand::Invite { mesh, .. }
            | IpcCommand::A2aCall { mesh, .. } => Some(mesh),
            IpcCommand::Info => None,
        }
    }
}

/// One IPC command plus the daemon's own identity (mesh/name/author) and app
/// state it's served against — grouped so `handle_ipc_command` stays within
/// the argument budget alongside its state/sender/output handles.
pub(crate) struct IpcDispatchParams<'a> {
    pub(crate) cmd: IpcCommand,
    pub(crate) resp_tx: oneshot::Sender<String>,
    pub(crate) mesh: &'a MeshId,
    pub(crate) name: &'a MeshName,
    pub(crate) author: &'a Nickname,
    pub(crate) app: &'a mut A2aApp,
}

/// Returns `true` if the handler broadcast anything, so the caller
/// can refresh `last_sent_at` for heartbeat suppression.
#[expect(
    clippy::too_many_lines,
    reason = "a dispatch match with one arm per IpcCommand (the state/meta channel pair doubles the patch/get arms)"
)]
pub(crate) async fn handle_ipc_command(
    params: IpcDispatchParams<'_>,
    state: &mut EventLoopState,
    sender: &MeshSender,
    output: &output::Output,
) -> bool {
    let IpcDispatchParams {
        cmd,
        resp_tx,
        mesh,
        name,
        author,
        app,
    } = params;
    // The per-mesh socket path already routes a command to the right daemon,
    // but a command carries its own mesh id — validate it matches ours rather
    // than binding it to `_` and trusting the path alone (a stale socket path or
    // a symlinked runtime dir would otherwise misroute a signed broadcast). The
    // `Info` probe carries no mesh and is addressed purely by socket path.
    if let Some(cmd_mesh) = cmd.mesh_id()
        && cmd_mesh != mesh
    {
        let _ = resp_tx.send(json_error("command gossip id does not match this daemon"));
        return false;
    }
    match cmd {
        IpcCommand::Broadcast { mesh: _, body } => {
            tracing::debug!("IPC broadcast command received");
            match send_broadcast(
                BroadcastParams {
                    mesh,
                    author,
                    text: body,
                },
                state,
                sender,
                output,
            )
            .await
            {
                Ok((msg_id, msg)) => {
                    let _ = resp_tx.send(json_ok_msg(&msg_id, &msg));
                    true
                }
                Err(error) => {
                    let _ = resp_tx.send(json_error(&error.to_string()));
                    false
                }
            }
        }
        IpcCommand::Msg { mesh: _, to, body } => {
            tracing::debug!("IPC msg command received");
            match send_msg(
                MsgParams {
                    mesh,
                    author,
                    peer: &to,
                    text: body,
                    app,
                },
                state,
                sender,
                output,
            )
            .await
            {
                Ok((msg_id, msg)) => {
                    let _ = resp_tx.send(json_ok_msg(&msg_id, &msg));
                    true
                }
                Err(error) => {
                    let _ = resp_tx.send(json_error(&error.to_string()));
                    false
                }
            }
        }
        IpcCommand::Poll {
            mesh: _,
            after,
            long,
        } => {
            // Surfaced-events ring, seq-cursored. Each event renders to the
            // *same* JSON line the live `--output json` stream emits (via
            // `surfaced_event_json`), with its `seq` flattened in so the client
            // advances `--after`. `poll_or_register` responds now if events are
            // buffered, else (with `long`) parks a waiter the loop fulfills
            // on the next surfaced event or expires at the park cap. A parked
            // waiter broadcasts nothing → `false`.
            app.surfaced.poll_or_register(PollOrRegisterParams {
                after,
                long,
                now: tokio::time::Instant::now(),
                responder: PollResponder::Json(resp_tx),
            });
            false
        }
        IpcCommand::Ping { mesh: _ } => {
            // Arm a fresh round (replacing any in flight) and broadcast
            // the probe. Pongs are collected by the gossip receive path;
            // the round's deadline drives the `ping_report` emission.
            let now = tokio::time::Instant::now();
            state.arm_ping_round(PingRound {
                t1: now,
                deadline: now + Duration::from_secs(ping_window_secs()),
                pongs: HashMap::new(),
                // CLI/IPC consumes the `ping_report` event, not a channel.
                resp: None,
            });
            broadcast_msg(
                sender,
                &Message::new_ping(mesh, author).signed(state.identity()),
            )
            .await;
            tracing::debug!("IPC ping command received; round armed");
            let _ = resp_tx.send(json_ack());
            true
        }
        IpcCommand::A2aStatus {
            mesh: _,
            task_id,
            state: task_state,
            note,
        } => {
            tracing::debug!(%task_id, ?task_state, "IPC a2a status command received");
            match emit_task_status(
                TaskStatusParams {
                    mesh,
                    author,
                    task_id: &task_id,
                    task_state,
                    note: note.as_deref(),
                    app,
                },
                state,
                sender,
                output,
            )
            .await
            {
                Ok(msg) => {
                    let _ = resp_tx.send(json_ok_msg(&msg.id.clone(), &msg));
                    true
                }
                Err(error) => {
                    let _ = resp_tx.send(json_error(&error.to_string()));
                    false
                }
            }
        }
        IpcCommand::A2aArtifact {
            mesh: _,
            task_id,
            text,
            file,
            file_name,
            file_mime,
        } => {
            tracing::debug!(%task_id, "IPC a2a artifact command received");
            let file = file.map(|path| crate::a2a::send::FileRef {
                path,
                name: file_name,
                mime: file_mime,
            });
            match emit_task_artifact(
                TaskArtifactEmitParams {
                    mesh,
                    author,
                    task_id: &task_id,
                    text: &text,
                    file,
                    app,
                },
                state,
                sender,
                output,
            )
            .await
            {
                Ok(msg) => {
                    let _ = resp_tx.send(json_ok_msg(&msg.id.clone(), &msg));
                    true
                }
                Err(error) => {
                    let _ = resp_tx.send(json_error(&error.to_string()));
                    false
                }
            }
        }
        IpcCommand::Peers { mesh: _ } => {
            let _ = resp_tx.send(peers_response(state));
            false
        }
        IpcCommand::StateMerge { mesh: _, merge } => {
            let outcome = broadcast_state_merge(
                state,
                StateMergeParams {
                    mesh,
                    author,
                    merge,
                    sender,
                    sink: output,
                    channel: fofoca::protocol::Channel::State,
                    surface: true,
                },
            )
            .await;
            let (response, broadcast) = state_merge_response(outcome.map(|_| ()));
            let _ = resp_tx.send(response);
            broadcast
        }
        IpcCommand::StateGet { mesh: _ } => {
            let _ = resp_tx.send(state_get_response(state, fofoca::protocol::Channel::State));
            false
        }
        IpcCommand::MetaMerge { mesh: _, merge } => {
            let outcome = broadcast_state_merge(
                state,
                StateMergeParams {
                    mesh,
                    author,
                    merge,
                    sender,
                    sink: output,
                    channel: fofoca::protocol::Channel::Meta,
                    surface: true,
                },
            )
            .await;
            let (response, broadcast) = state_merge_response(outcome.map(|_| ()));
            let _ = resp_tx.send(response);
            broadcast
        }
        IpcCommand::MetaGet { mesh: _ } => {
            let _ = resp_tx.send(state_get_response(state, fofoca::protocol::Channel::Meta));
            false
        }
        IpcCommand::Topology { mesh: _ } => {
            let _ = resp_tx.send(topology_response(state));
            false
        }
        IpcCommand::A2aCall {
            mesh: _,
            to,
            method,
            params: rpc_params,
            timeout_secs,
        } => {
            // The response arrives later (the peer's `A2aResp`, or a timeout),
            // so park `resp_tx` in the waiter — like the long-poll `Poll` arm —
            // and answer nothing now.
            crate::a2a::send::broadcast_a2a_call(
                crate::a2a::send::BroadcastA2aCallParams {
                    mesh,
                    author,
                    peer: to,
                    method: &method,
                    params: rpc_params,
                    timeout: Duration::from_secs(timeout_secs),
                    responder: crate::a2a::app::A2aResponder::Ipc(resp_tx),
                },
                state,
                app,
                sender,
            )
            .await;
            true
        }
        IpcCommand::Info => {
            let _ = resp_tx.send(info_response(mesh, name, author, state));
            false
        }
        IpcCommand::Invite { mesh: _, ttl } => {
            let _ = resp_tx.send(invite_response(state, ttl));
            false
        }
    }
}

/// Mint an invite for the mesh this daemon serves — creator-only. The
/// issuer key lives in `state.mint_mesh()` (populated only on the creator of an
/// invite-only mesh); every other session refuses.
fn invite_response(state: &EventLoopState, ttl: u64) -> String {
    let Some(mesh) = &state.mint_mesh() else {
        return serde_json::json!({
            "ok": false,
            "error": "invites can only be minted by the creator of an invite-only gossip",
        })
        .to_string();
    };
    match fofoca::ops::invite::mint(mesh, Some(ttl), state.mesh_password()) {
        Ok(token) => serde_json::json!({ "ok": true, "invite": token }).to_string(),
        Err(error) => serde_json::json!({ "ok": false, "error": error.to_string() }).to_string(),
    }
}

/// The `doctor` identity probe response: this daemon's own mesh id, human
/// name, nickname, and mesh size. `peer_count` matches the field name
/// `peers` / the state file / MCP `gossip_info` already use for mesh size
/// (roster peers + 1 for self).
fn info_response(
    mesh: &MeshId,
    name: &MeshName,
    author: &Nickname,
    state: &EventLoopState,
) -> String {
    serde_json::json!({
        "ok": true,
        "gossip": mesh.as_str(),
        "name": name.as_str(),
        "nickname": author.as_str(),
        "peer_count": state.roster_snapshot().count,
    })
    .to_string()
}

/// Map a state-merge outcome to its IPC response JSON and whether anything was
/// broadcast (so the caller can refresh the heartbeat clock). A merge always
/// applies, so `Ok` is the broadcast/ack and `Err` is a transport failure.
fn state_merge_response(outcome: anyhow::Result<()>) -> (String, bool) {
    match outcome {
        Ok(()) => (json_ack(), true),
        Err(error) => (json_error(&error.to_string()), false),
    }
}

/// Serialize this daemon's multihop routing topology (its assembled mesh graph)
/// for the `topology` IPC query. `{"ok":true,"topology":{self_id, edges:[…]}}`.
/// Empty when the multihop transport is off (no routing table).
fn topology_response(state: &EventLoopState) -> String {
    let Some(handle) = state.multihop() else {
        return r#"{"ok":true,"topology":{"self_id":"","edges":[]}}"#.to_owned();
    };
    let topology = handle.topology_view();
    let topo_json = serde_json::to_string(&topology).unwrap_or_else(|_| "null".to_owned());
    format!(r#"{{"ok":true,"topology":{topo_json}}}"#)
}

/// The `agent-gossip state get` response: the derived document.
fn state_get_response(state: &EventLoopState, channel: fofoca::protocol::Channel) -> String {
    let document = state.doc(channel).to_json();
    let doc_json = serde_json::to_string(&document).unwrap_or_else(|_| "null".to_owned());
    format!(r#"{{"ok":true,"document":{doc_json}}}"#)
}

/// Serialize the live roster snapshot as the `agent-gossip peers` response.
/// `ok:true` plus the snapshot's `peers` (recency-sorted, peers only)
/// and `peer_count` (`peers.len() + 1` — the `+1` is self, so
/// the count is mesh size, not the array length). Matches the field name the
/// MCP `gossip_info` result and the state file already use for this quantity.
fn peers_response(state: &EventLoopState) -> String {
    let snapshot = state.roster_snapshot();
    serde_json::json!({
        "ok": true,
        "peers": snapshot.peers,
        "peer_count": snapshot.count,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::{IpcCommand, MeshId, MessageBody, Nickname, TaskId, TaskState};
    use fofoca::runtime::ipc::Addressed;

    // ── IpcCommand serialization ───────────────────────────────────
    //
    // The a2a-typed command set lives app-side; these guard its wire format
    // (the CLI IPC protocol) — the engine's generic socket server names none
    // of it.

    #[test]
    fn ipc_command_msg_round_trip() {
        let expected = MeshId::from("test");
        let cmd = IpcCommand::Broadcast {
            mesh: expected.clone(),
            body: MessageBody::from("hello"),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed.mesh_id().expect("Msg is mesh-addressed").as_str(),
            expected.as_str()
        );
    }

    #[test]
    fn ipc_command_info_round_trip() {
        let json = serde_json::to_string(&IpcCommand::Info).unwrap();
        assert_eq!(json, r#"{"command":"info"}"#);
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        assert!(parsed.mesh_id().is_none(), "Info carries no mesh");
    }

    #[test]
    fn ipc_command_state_merge_round_trip() {
        let expected = MeshId::from("test");
        let cmd = IpcCommand::StateMerge {
            mesh: expected.clone(),
            merge: serde_json::json!({"turn": "b"}),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""command":"state_merge""#), "tag: {json}");
        assert!(json.contains(r#""merge""#), "{json}");
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::StateMerge { mesh, merge } => {
                assert_eq!(mesh, expected);
                assert_eq!(merge, serde_json::json!({"turn": "b"}));
            }
            IpcCommand::Broadcast { .. }
            | IpcCommand::Msg { .. }
            | IpcCommand::Poll { .. }
            | IpcCommand::Ping { .. }
            | IpcCommand::A2aStatus { .. }
            | IpcCommand::A2aArtifact { .. }
            | IpcCommand::Peers { .. }
            | IpcCommand::MetaMerge { .. }
            | IpcCommand::MetaGet { .. }
            | IpcCommand::StateGet { .. }
            | IpcCommand::A2aCall { .. }
            | IpcCommand::Topology { .. }
            | IpcCommand::Invite { .. }
            | IpcCommand::Info => panic!("expected StateMerge"),
        }
    }

    #[test]
    fn ipc_command_poll_round_trip() {
        let cmd = IpcCommand::Poll {
            mesh: MeshId::from("test"),
            after: Some(42),
            long: false,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        // `long: false` is skipped on the wire, keeping the format byte-stable
        // for callers that never long-poll.
        assert!(!json.contains("long"), "false long must not serialize");
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::Poll { after, long, .. } => {
                assert_eq!(after, Some(42));
                assert!(!long, "absent long deserializes to false");
            }
            IpcCommand::Broadcast { .. }
            | IpcCommand::Msg { .. }
            | IpcCommand::Ping { .. }
            | IpcCommand::A2aStatus { .. }
            | IpcCommand::A2aArtifact { .. }
            | IpcCommand::Peers { .. }
            | IpcCommand::StateMerge { .. }
            | IpcCommand::MetaMerge { .. }
            | IpcCommand::MetaGet { .. }
            | IpcCommand::StateGet { .. }
            | IpcCommand::A2aCall { .. }
            | IpcCommand::Topology { .. }
            | IpcCommand::Invite { .. }
            | IpcCommand::Info => panic!("expected Poll"),
        }
    }

    #[test]
    fn ipc_command_ping_round_trip() {
        let expected = MeshId::from("test");
        let cmd = IpcCommand::Ping {
            mesh: expected.clone(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"command\":\"ping\""));
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::Ping { mesh } => assert_eq!(mesh, expected),
            IpcCommand::Broadcast { .. }
            | IpcCommand::Msg { .. }
            | IpcCommand::Poll { .. }
            | IpcCommand::A2aStatus { .. }
            | IpcCommand::A2aArtifact { .. }
            | IpcCommand::Peers { .. }
            | IpcCommand::StateMerge { .. }
            | IpcCommand::MetaMerge { .. }
            | IpcCommand::MetaGet { .. }
            | IpcCommand::StateGet { .. }
            | IpcCommand::A2aCall { .. }
            | IpcCommand::Topology { .. }
            | IpcCommand::Invite { .. }
            | IpcCommand::Info => panic!("expected Ping"),
        }
    }

    #[test]
    fn ipc_command_a2a_status_round_trip() {
        let cmd = IpcCommand::A2aStatus {
            mesh: MeshId::from("test"),
            task_id: TaskId::from("550e8400-e29b-41d4-a716-446655440000"),
            state: TaskState::Working,
            note: Some("on it".to_string()),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"command\":\"a2a_status\""));
        assert!(json.contains("\"state\":\"working\""));
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::A2aStatus { state, note, .. } => {
                assert_eq!(state, TaskState::Working);
                assert_eq!(note.as_deref(), Some("on it"));
            }
            IpcCommand::Broadcast { .. }
            | IpcCommand::Msg { .. }
            | IpcCommand::Poll { .. }
            | IpcCommand::Ping { .. }
            | IpcCommand::A2aArtifact { .. }
            | IpcCommand::Peers { .. }
            | IpcCommand::StateMerge { .. }
            | IpcCommand::MetaMerge { .. }
            | IpcCommand::MetaGet { .. }
            | IpcCommand::StateGet { .. }
            | IpcCommand::A2aCall { .. }
            | IpcCommand::Topology { .. }
            | IpcCommand::Invite { .. }
            | IpcCommand::Info => panic!("expected A2aStatus"),
        }
    }

    #[test]
    fn ipc_command_peers_round_trip() {
        let expected = MeshId::from("test");
        let cmd = IpcCommand::Peers {
            mesh: expected.clone(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"command\":\"peers\""));
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::Peers { mesh } => assert_eq!(mesh, expected),
            IpcCommand::Broadcast { .. }
            | IpcCommand::Msg { .. }
            | IpcCommand::Poll { .. }
            | IpcCommand::Ping { .. }
            | IpcCommand::A2aStatus { .. }
            | IpcCommand::A2aArtifact { .. }
            | IpcCommand::StateMerge { .. }
            | IpcCommand::MetaMerge { .. }
            | IpcCommand::MetaGet { .. }
            | IpcCommand::StateGet { .. }
            | IpcCommand::A2aCall { .. }
            | IpcCommand::Topology { .. }
            | IpcCommand::Invite { .. }
            | IpcCommand::Info => panic!("expected Peers"),
        }
    }

    #[test]
    fn ipc_command_poll_no_after_skips_field() {
        let cmd = IpcCommand::Poll {
            mesh: MeshId::from("test"),
            after: None,
            long: false,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(!json.contains("after"));
        assert!(!json.contains("long"));
    }

    #[test]
    fn ipc_command_poll_long_serializes_true() {
        let cmd = IpcCommand::Poll {
            mesh: MeshId::from("test"),
            after: None,
            long: true,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"long\":true"), "wire: {json}");
    }

    mod prop {
        use proptest::collection::vec as arb_vec;
        use proptest::{prop_assert, prop_assert_eq, proptest, strategy::Strategy};

        use super::{MeshId, MessageBody, Nickname};

        fn arb_ascii_body() -> impl Strategy<Value = String> {
            arb_vec(0x20u8..0x7Eu8, 0..200).prop_map(|bytes| String::from_utf8(bytes).unwrap())
        }

        fn arb_nickname() -> impl Strategy<Value = String> {
            "[a-z]{3,8}-[a-z]{3,8}"
        }

        fn arb_mesh() -> impl Strategy<Value = MeshId> {
            "[1-9A-HJ-NP-Za-km-z]{4,24}".prop_map(|label| MeshId::from(label.as_str()))
        }

        proptest! {
            #![proptest_config(crate::proptest_support::config())]
            // ── Round-trip: build_msg_bytes -> Message::parse ──────

            #[test]
            fn prop_build_msg_bytes_message_round_trip(
                mesh in arb_mesh(),
                body in arb_ascii_body(),
                author in arb_nickname(),
            ) {
                let author = Nickname::new(author).unwrap();
                let body = MessageBody::new(body).unwrap();
                let expected_body = body.clone();
                let identity = fofoca::protocol::Identity::generate();
                let (bytes, built) = fofoca::protocol::build_msg_bytes(
                    fofoca::protocol::BuildMsgParams {
                        tag: fofoca::protocol::AppTag::from(crate::a2a::wire::BROADCAST),
                        mesh: &mesh,
                        author: &author,
                        body,
                        chain: fofoca::protocol::ChainCtx::genesis(),
                    },
                    &identity,
                )
                .unwrap();
                prop_assert!(!built.id.as_str().is_empty());
                let parsed = fofoca::protocol::Message::parse(&bytes).unwrap();
                prop_assert_eq!(&parsed.author, &author);
                prop_assert_eq!(&parsed.body, &expected_body);
                prop_assert_eq!(&parsed.mesh, &mesh);
                prop_assert_eq!(
                    parsed.kind,
                    fofoca::protocol::MessageKind::app_broadcast(crate::a2a::wire::BROADCAST)
                );
            }
        }
    }
}
