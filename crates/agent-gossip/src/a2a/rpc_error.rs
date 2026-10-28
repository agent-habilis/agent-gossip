use habilis_network::embed::EventLoopState;
use habilis_network::ops::{MergeCtx, MeshSender, StateMergeParams, broadcast_state_merge};
use habilis_network::protocol::{AuthorKey, Channel, MeshId, MessageBody, Nickname};
use serde_json::Value;
use tokio::sync::oneshot;

use super::TaskId;
use super::extensions::META_WAITING;
use super::task_record::{TaskRecord, TaskRole};
use crate::a2a::a2a_app::A2aApp;
use crate::a2a::broadcast_params::{
    BroadcastParams, emit_task_status, log_pending, send_broadcast,
};
use crate::output;

/// A JSON-RPC error (code + message), mapped to the A2A error space.
#[derive(Debug)]
#[must_use]
pub(crate) struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub(crate) fn invalid_params(message: &str) -> Self {
        RpcError {
            code: -32602,
            message: message.to_owned(),
        }
    }

    /// A2A `TaskNotFoundError`. Also the answer a gossip caller gets for a
    /// task that exists but is not theirs, so the error cannot be used to
    /// probe which task ids the peer holds.
    pub(crate) fn task_not_found(task_id: &TaskId) -> Self {
        RpcError {
            code: -32001,
            message: format!("task not found: {task_id}"),
        }
    }

    pub(crate) fn internal(message: &str) -> Self {
        RpcError {
            code: -32603,
            message: message.to_owned(),
        }
    }

    /// A2A method-not-found (`-32601`).
    pub(crate) fn method_not_found(method: &str) -> Self {
        RpcError {
            code: -32601,
            message: format!("method not found: {method}"),
        }
    }

    /// The method exists but is not served over the gossip binding
    /// (it would author under the serving peer's identity).
    pub(crate) fn not_permitted(method: &str) -> Self {
        RpcError {
            code: -32003,
            message: format!("not permitted over gossip: {method}"),
        }
    }
}

/// One A2A operation the HTTP binding hands to the event loop. The URL names
/// the target agent (`to: None` = the mesh-collective broadcast endpoint,
/// `Some(nick)` = that peer), per the A2A rule that the endpoint
/// identifies the agent.
pub(crate) enum A2aOp {
    SendMessage {
        to: Option<Nickname>,
        message: super::Message,
    },
    GetTask {
        task_id: TaskId,
    },
    /// `Some(peer)` restricts the listing to tasks whose counterparty is
    /// `peer` — what a remote caller over gossip may see. `None` is the local
    /// binding, where the caller owns the daemon and sees every task.
    ListTasks {
        peer: Option<Nickname>,
    },
    CancelTask {
        task_id: TaskId,
    },
    /// The `mesh-state` extension methods (`mesh/state.get` etc.).
    ChannelGet {
        channel: Channel,
    },
    ChannelMerge {
        channel: Channel,
        merge: Value,
    },
    OwnCard,
    PeerCard {
        peer: Nickname,
    },
}

/// One in-flight request from the HTTP task to the event loop.
pub(crate) struct A2aRequest {
    pub op: A2aOp,
    pub response: oneshot::Sender<Result<Value, RpcError>>,
}

/// Map a JSON-RPC `method` + `params` (+ the URL-derived target) to an op.
///
/// # Errors
/// Unknown method (`-32601`) or malformed params (`-32602`).
pub(crate) fn parse_op(
    method: &str,
    params: &Value,
    target: Option<Nickname>,
) -> Result<A2aOp, RpcError> {
    let task_id_param = || {
        params["id"]
            .as_str()
            .and_then(TaskId::from_uuid_str)
            .ok_or_else(|| RpcError::invalid_params("params.id must be a task id (uuid)"))
    };
    match method {
        // `SendStreamingMessage` shares `SendMessage`'s create semantics; the
        // streaming events ride the worker's pushed status/artifact frames (the
        // gossip push plane). The SSE `text/event-stream` edge encoding for an
        // off-the-shelf HTTP client is layered on top of that plane.
        "SendMessage" | "SendStreamingMessage" => {
            let message: super::Message = serde_json::from_value(params["message"].clone())
                .map_err(|error| {
                    RpcError::invalid_params(&format!(
                        "params.message is not an A2A Message: {error}"
                    ))
                })?;
            Ok(A2aOp::SendMessage {
                to: target,
                message,
            })
        }
        // Subscribe returns the current snapshot; the caller (a party) keeps
        // receiving the worker's pushed frames.
        "GetTask" | "SubscribeToTask" => Ok(A2aOp::GetTask {
            task_id: task_id_param()?,
        }),
        "ListTasks" => Ok(A2aOp::ListTasks { peer: None }),
        "CancelTask" => Ok(A2aOp::CancelTask {
            task_id: task_id_param()?,
        }),
        // The authenticated extended card. We have nothing extra to gate behind
        // auth, so it returns the same full card `own_card` builds.
        "GetExtendedAgentCard" => Ok(A2aOp::OwnCard),
        "mesh/state.get" => Ok(A2aOp::ChannelGet {
            channel: Channel::State,
        }),
        "mesh/meta.get" => Ok(A2aOp::ChannelGet {
            channel: Channel::Meta,
        }),
        "mesh/state.merge" | "mesh/meta.merge" => {
            let channel = if method == "mesh/state.merge" {
                Channel::State
            } else {
                Channel::Meta
            };
            if params["merge"].is_null() {
                return Err(RpcError::invalid_params("params.merge is required"));
            }
            Ok(A2aOp::ChannelMerge {
                channel,
                merge: params["merge"].clone(),
            })
        }
        // Streaming and push-notification configs are declared off in the
        // card (`capabilities`); a client that calls them anyway gets the
        // standard method-not-found.
        other => Err(RpcError {
            code: -32601,
            message: format!("method not found: {other}"),
        }),
    }
}

/// The A2A `Task` object for a live record — what `tasks/get`/`tasks/list`
/// return and what a task-creating `message/send` echoes. History/artifact
/// accumulation is not stored daemon-side (the event stream carries them);
/// mesh-specific coordinates ride `metadata`.
/// The `ListTasks` payload. `peer` scopes the listing to that counterparty's
/// tasks — what a remote caller may see; `None` lists everything, for the
/// local binding whose caller owns the daemon.
fn list_tasks(app: &A2aApp, peer: Option<&Nickname>, mesh: &MeshId) -> Value {
    // Sort by task id so the response order is stable across calls
    // (`app.tasks` is a HashMap with unspecified iteration order).
    let mut entries: Vec<(&TaskId, &TaskRecord)> = app
        .tasks
        .iter()
        .filter(|(_, rec)| peer.is_none_or(|peer| rec.peer == *peer))
        .collect();
    entries.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
    let tasks: Vec<Value> = entries
        .into_iter()
        .map(|(task_id, rec)| task_object(task_id, rec, mesh))
        .collect();
    serde_json::json!({ "tasks": tasks })
}

pub(crate) fn task_object(task_id: &TaskId, rec: &TaskRecord, mesh: &MeshId) -> Value {
    let role = |value: TaskRole| match value {
        TaskRole::Initiator => "initiator",
        TaskRole::Receiver => "worker",
    };
    let mut metadata = serde_json::json!({
        "mesh:peer": rec.peer.as_str(),
        "mesh:role": role(rec.role),
        "mesh:ball": role(rec.ball),
        "mesh:review": rec.review,
    });
    if let Some(reason) = &rec.waiting {
        metadata[META_WAITING] = Value::from(reason.as_str());
    }
    serde_json::json!({
        "id": task_id.as_str(),
        "contextId": mesh.as_str(),
        "status": { "state": rec.state },
        "metadata": metadata,
    })
}

/// A reaped task answers with its last state and peer only: the record is gone,
/// and keeping the role and review flag would grow the closed set.
pub(crate) fn get_task(app: &A2aApp, task_id: &TaskId, mesh: &MeshId) -> Result<Value, RpcError> {
    if let Some(rec) = app.tasks.get(task_id) {
        return Ok(task_object(task_id, rec, mesh));
    }
    app.closed_tasks
        .get(task_id)
        .map(|closed| {
            serde_json::json!({
                "id": task_id.as_str(),
                "contextId": mesh.as_str(),
                "status": { "state": closed.state },
                "metadata": { "mesh:peer": closed.peer.as_str() },
            })
        })
        .ok_or_else(|| RpcError::task_not_found(task_id))
}

/// One JSON-RPC op's identity (mesh/author/pubkey/local port) plus the app
/// state it's resolved against — grouped so `handle_op` stays within the
/// argument budget alongside its state/sender/output handles.
pub(crate) struct HandleOpParams<'caller> {
    pub(crate) op: A2aOp,
    pub(crate) mesh: &'caller MeshId,
    pub(crate) author: &'caller Nickname,
    pub(crate) our_pubkey: AuthorKey,
    pub(crate) a2a_port: u16,
    pub(crate) app: &'caller mut A2aApp,
}

/// Execute one op against the live loop state — the JSON-RPC binding's
/// dispatch, sharing the exact broadcast paths the IPC socket uses so the
/// two bindings cannot drift.
pub(crate) async fn handle_op(
    params: HandleOpParams<'_>,
    state: &mut EventLoopState,
    sender: &MeshSender,
    output: &output::Output,
) -> Result<Value, RpcError> {
    let HandleOpParams {
        op,
        mesh,
        author,
        our_pubkey,
        a2a_port,
        app,
    } = params;
    match op {
        A2aOp::SendMessage { to, message } => {
            send_message(
                SendMessageParams {
                    to,
                    message,
                    mesh,
                    author,
                },
                state,
                sender,
                output,
            )
            .await
        }
        A2aOp::GetTask { task_id } => get_task(app, &task_id, mesh),
        A2aOp::ListTasks { peer } => Ok(list_tasks(app, peer.as_ref(), mesh)),
        A2aOp::CancelTask { task_id } => {
            if !app.tasks.contains_key(&task_id) {
                return Err(RpcError::task_not_found(&task_id));
            }
            let (_echo, pending) = emit_task_status(
                crate::a2a::broadcast_params::TaskStatusParams {
                    mesh,
                    author,
                    task_id: &task_id,
                    task_state: super::TaskState::Canceled,
                    note: Some("canceled"),
                    waiting: None,
                    app,
                },
                state,
                sender,
                output,
            )
            .await
            .map_err(|error| RpcError::internal(&error.to_string()))?;
            // The task is canceled in our record whether or not the peer is
            // reached; awaiting the dial here would stop the event loop.
            log_pending(pending);
            app.tasks
                .get(&task_id)
                .map(|rec| task_object(&task_id, rec, mesh))
                .ok_or_else(|| RpcError::task_not_found(&task_id))
        }
        A2aOp::ChannelGet { channel } => Ok(state.doc(channel).to_json()),
        A2aOp::ChannelMerge { channel, merge } => {
            broadcast_state_merge(
                state,
                StateMergeParams::new(MergeCtx::new(mesh, author, sender, output), channel, merge),
            )
            .await
            .map_err(|error| RpcError::internal(&error.to_string()))?;
            Ok(serde_json::json!({ "ok": true }))
        }
        A2aOp::OwnCard => {
            let seal_b58 = bs58::encode(state.identity().seal_public().as_bytes()).into_string();
            let mut card = super::card::own_card(author, our_pubkey, &seal_b58);
            // Served over the localhost binding, so advertise the JSONRPC
            // interface alongside the always-present gossip one.
            card.supported_interfaces.push(super::AgentInterface {
                url: format!("http://127.0.0.1:{a2a_port}/"),
                protocol_binding: "JSONRPC".to_string(),
                tenant: None,
                protocol_version: super::PROTOCOL_VERSION.to_string(),
            });
            serde_json::to_value(&card).map_err(|error| RpcError::internal(&error.to_string()))
        }
        A2aOp::PeerCard { peer } => {
            let doc = state.doc(Channel::Meta).to_json();
            // A card that is still arriving, in pieces, is not served.
            let mut card = super::card::complete_card(&doc, &peer)
                .cloned()
                .unwrap_or(Value::Null);
            if card.is_null() {
                return Err(RpcError {
                    code: -32004,
                    message: format!("no card published for '{peer}'"),
                });
            }
            // The mesh card declares only its gossip interface (the peer has no
            // HTTP endpoint of its own). Add a relaying JSONRPC interface at
            // *our* binding — we relay JSON-RPC to that peer over gossip at
            // `/peers/<nick>` — so the served peer card is reachable (e.g.
            // `SendMessage` there creates a task on the peer).
            if let Some(interfaces) = card
                .pointer_mut("/supportedInterfaces")
                .and_then(Value::as_array_mut)
            {
                interfaces.push(serde_json::json!({
                    "url": format!("http://127.0.0.1:{a2a_port}/peers/{peer}"),
                    "protocolBinding": "JSONRPC",
                    "protocolVersion": super::PROTOCOL_VERSION,
                }));
            }
            Ok(card)
        }
    }
}

/// The addressee (must be `None`), payload, and identity of one
/// `message/send` RPC op — grouped so `send_message` stays within the
/// argument budget alongside its state/sender/output handles.
struct SendMessageParams<'caller> {
    to: Option<Nickname>,
    message: super::Message,
    mesh: &'caller MeshId,
    author: &'caller Nickname,
}

/// A **broadcast** `message/send` (no addressee): re-author the wire payload
/// from the client Message's text projection (role/context/extension stamping
/// stays the daemon's job) and flood it. A *directed* `message/send` (task
/// creation) never reaches here — the localhost dispatch (`handle_a2a_arm`)
/// intercepts it and routes it through the gossip request/response waiter, and
/// the gossip serve path ingests it directly — so a `Some(to)` here is a bug.
async fn send_message(
    params: SendMessageParams<'_>,
    state: &mut EventLoopState,
    sender: &MeshSender,
    output: &output::Output,
) -> Result<Value, RpcError> {
    let SendMessageParams {
        to,
        message,
        mesh,
        author,
    } = params;
    if to.is_some() {
        return Err(RpcError::internal(
            "directed message/send should be routed through the request/response waiter",
        ));
    }
    let text = super::status_update_params::display_text(&message);
    let body = MessageBody::try_new(text)
        .map_err(|error| RpcError::invalid_params(&format!("message text: {error}")))?;
    let (_id, frame) = send_broadcast(
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
    .map_err(|error| RpcError::internal(&error.to_string()))?;
    // A2A v1.0 `SendMessage` returns a `SendMessageResponse` oneof; a broadcast
    // has no task, so echo the daemon-authored Message as `{"message": …}`.
    let echo: super::Message = serde_json::from_str(frame.body.as_str())
        .map_err(|error| RpcError::internal(&error.to_string()))?;
    serde_json::to_value(super::SendMessageResponse::Message(echo))
        .map_err(|error| RpcError::internal(&error.to_string()))
}
