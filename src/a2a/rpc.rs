use serde_json::Value;
use tokio::sync::oneshot;

use crate::daemon::ctx::HandlerCtx;
use crate::daemon::state::EventLoopState;
use crate::gossip::{broadcast_message, broadcast_state_merge};
use crate::output;
use crate::protocol::swarm::SwarmId;
use crate::protocol::{Channel, MessageBody, Nickname};

use super::{TaskId, task::TaskRecord, task::TaskRole};

/// A JSON-RPC error (code + message), mapped to the A2A error space.
#[derive(Debug)]
pub(crate) struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub(crate) fn invalid_params(message: impl Into<String>) -> Self {
        RpcError {
            code: -32602,
            message: message.into(),
        }
    }

    /// A2A `TaskNotFoundError`.
    fn task_not_found(task_id: &TaskId) -> Self {
        RpcError {
            code: -32001,
            message: format!("task not found: {task_id}"),
        }
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        RpcError {
            code: -32603,
            message: message.into(),
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
/// the target agent (`to: None` = the swarm-collective broadcast endpoint,
/// `Some(nick)` = that participant), per the A2A rule that the endpoint
/// identifies the agent.
pub(crate) enum A2aOp {
    SendMessage {
        to: Option<Nickname>,
        message: super::Message,
    },
    GetTask {
        task_id: TaskId,
    },
    ListTasks,
    CancelTask {
        task_id: TaskId,
    },
    /// The `swarm-state` extension methods (`swarm/state.get` etc.).
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
    pub resp: oneshot::Sender<Result<Value, RpcError>>,
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
                    RpcError::invalid_params(format!(
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
        "ListTasks" => Ok(A2aOp::ListTasks),
        "CancelTask" => Ok(A2aOp::CancelTask {
            task_id: task_id_param()?,
        }),
        // The authenticated extended card. We have nothing extra to gate behind
        // auth, so it returns the same full card `own_card` builds.
        "GetExtendedAgentCard" => Ok(A2aOp::OwnCard),
        "swarm/state.get" => Ok(A2aOp::ChannelGet {
            channel: Channel::State,
        }),
        "swarm/meta.get" => Ok(A2aOp::ChannelGet {
            channel: Channel::Meta,
        }),
        "swarm/state.merge" | "swarm/meta.merge" => {
            let channel = if method == "swarm/state.merge" {
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
/// swarm-specific coordinates ride `metadata`.
pub(crate) fn task_object(task_id: &TaskId, rec: &TaskRecord, swarm: &SwarmId) -> Value {
    let role = |value: TaskRole| match value {
        TaskRole::Initiator => "initiator",
        TaskRole::Receiver => "worker",
    };
    serde_json::json!({
        "id": task_id.as_str(),
        "contextId": swarm.as_str(),
        "status": { "state": rec.state },
        "metadata": {
            "swarm:peer": rec.peer.as_str(),
            "swarm:role": role(rec.role),
            "swarm:ball": role(rec.ball),
            "swarm:review": rec.review,
        },
    })
}

/// Execute one op against the live loop state — the JSON-RPC binding's
/// dispatch, sharing the exact broadcast paths the IPC socket uses so the
/// two bindings cannot drift.
pub(crate) async fn handle_op(
    op: A2aOp,
    ctx: &HandlerCtx<'_>,
    a2a_port: u16,
    state: &mut EventLoopState,
) -> Result<Value, RpcError> {
    match op {
        A2aOp::SendMessage { to, message } => {
            send_message(
                to, message, ctx.swarm, ctx.author, state, ctx.sender, ctx.output,
            )
            .await
        }
        A2aOp::GetTask { task_id } => state
            .tasks
            .get(&task_id)
            .map(|rec| task_object(&task_id, rec, ctx.swarm))
            .ok_or_else(|| RpcError::task_not_found(&task_id)),
        A2aOp::ListTasks => {
            // Sort by task id so the response order is stable across calls
            // (`state.tasks` is a HashMap with unspecified iteration order).
            let mut entries: Vec<(&TaskId, &TaskRecord)> = state.tasks.iter().collect();
            entries.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
            let tasks: Vec<Value> = entries
                .into_iter()
                .map(|(task_id, rec)| task_object(task_id, rec, ctx.swarm))
                .collect();
            Ok(serde_json::json!({ "tasks": tasks }))
        }
        A2aOp::CancelTask { task_id } => {
            if !state.tasks.contains_key(&task_id) {
                return Err(RpcError::task_not_found(&task_id));
            }
            crate::gossip::emit_task_status(
                ctx,
                &task_id,
                super::TaskState::Canceled,
                Some("canceled"),
                state,
            )
            .await
            .map_err(|error| RpcError::internal(error.to_string()))?;
            state
                .tasks
                .get(&task_id)
                .map(|rec| task_object(&task_id, rec, ctx.swarm))
                .ok_or_else(|| RpcError::task_not_found(&task_id))
        }
        A2aOp::ChannelGet { channel } => {
            let document = match channel {
                Channel::State => state.state_doc.to_json(),
                Channel::Meta => state.meta_doc.to_json(),
            };
            Ok(document)
        }
        A2aOp::ChannelMerge { channel, merge } => {
            broadcast_state_merge(ctx, merge, state, channel, true)
                .await
                .map_err(|error| RpcError::internal(error.to_string()))?;
            Ok(serde_json::json!({ "ok": true }))
        }
        A2aOp::OwnCard => {
            let seal_b58 = bs58::encode(state.identity.seal_public()).into_string();
            let mut card = super::card::own_card(ctx.author, ctx.our_pubkey, &seal_b58);
            // Served over the localhost binding, so advertise the JSONRPC
            // interface alongside the always-present gossip one.
            card.supported_interfaces.push(super::AgentInterface {
                url: format!("http://127.0.0.1:{a2a_port}/"),
                protocol_binding: "JSONRPC".to_string(),
                tenant: None,
                protocol_version: super::PROTOCOL_VERSION.to_string(),
            });
            serde_json::to_value(&card).map_err(|error| RpcError::internal(error.to_string()))
        }
        A2aOp::PeerCard { peer } => {
            let doc = state.meta_doc.to_json();
            let mut card = doc
                .pointer(&format!("/peers/{peer}/card"))
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

/// A **broadcast** `message/send` (no addressee): re-author the wire payload
/// from the client Message's text projection (role/context/extension stamping
/// stays the daemon's job) and flood it. A *directed* `message/send` (task
/// creation) never reaches here — the localhost dispatch (`handle_a2a_arm`)
/// intercepts it and routes it through the gossip request/response waiter, and
/// the gossip serve path ingests it directly — so a `Some(to)` here is a bug.
async fn send_message(
    to: Option<Nickname>,
    message: super::Message,
    swarm: &SwarmId,
    author: &Nickname,
    state: &mut EventLoopState,
    sender: &crate::transport::SwarmSender,
    output: &output::Output,
) -> Result<Value, RpcError> {
    if to.is_some() {
        return Err(RpcError::internal(
            "directed message/send should be routed through the request/response waiter",
        ));
    }
    let text = super::gossip::display_text(&message);
    let body = MessageBody::new(text)
        .map_err(|error| RpcError::invalid_params(format!("message text: {error}")))?;
    let (_id, frame) = broadcast_message(swarm, author, body, state, sender, output)
        .await
        .map_err(|error| RpcError::internal(error.to_string()))?;
    // A2A v1.0 `SendMessage` returns a `SendMessageResponse` oneof; a broadcast
    // has no task, so echo the daemon-authored Message as `{"message": …}`.
    let echo: super::Message = serde_json::from_str(frame.body.as_str())
        .map_err(|error| RpcError::internal(error.to_string()))?;
    serde_json::to_value(super::SendMessageResponse::Message(echo))
        .map_err(|error| RpcError::internal(error.to_string()))
}
