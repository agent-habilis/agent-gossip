use habilis_network::embed::{EventLoopState, HandlerCtx, KeyOfError};
use habilis_network::ops::{MergeCtx, MeshSender, StateMergeParams, broadcast_state_merge};
use habilis_network::protocol::{AuthorKey, Channel, MeshId, MessageBody, Nickname};
use serde_json::Value;
use tokio::sync::oneshot;

use super::extensions::{
    META_LABEL, META_PROGRESS, META_SEQ, META_STATUS_TEXT, META_STATUS_TRUNCATED, META_UNCONFIRMED,
    META_WAITING_USER,
};
use super::task_record::{TaskRecord, TaskRole};
use super::{TaskId, TaskState};
use crate::a2a::a2a_app::A2aApp;
use crate::a2a::broadcast_params::{
    BroadcastParams, emit_task_status, log_pending, send_broadcast,
};
use crate::a2a::state_refusal_reply::StateRefusalReply;
use crate::emission;

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

    /// A2A's `TaskNotCancelableError`: the task is over, so there is nothing left to cancel.
    pub(crate) fn task_not_cancelable(task_id: &TaskId, state: TaskState) -> Self {
        RpcError {
            code: -32002,
            message: format!("task {task_id} is already {state} and cannot be canceled"),
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
        to: Option<PeerRef>,
        message: super::Message,
    },
    GetTask {
        task_id: TaskId,
    },
    /// `Some(peer)` restricts the listing to tasks whose counterparty is
    /// `peer` — what a remote caller over gossip may see. `None` is the local
    /// binding, where the caller owns the daemon and sees every task.
    ListTasks {
        peer: Option<AuthorKey>,
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
        peer: PeerRef,
    },
}

/// The member that a peer route names. A nickname is a label that two members can share, so the
/// route also takes the exact author key.
pub(crate) enum PeerRef {
    Nickname(Nickname),
    Key(AuthorKey),
}

impl PeerRef {
    /// The member that the last segment of a peer route names: the hex of an author key first,
    /// else a nickname. `None` if it is neither.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        match AuthorKey::decode_hex(text) {
            Some(key) => Some(Self::Key(key)),
            None => Nickname::try_new(text).ok().map(Self::Nickname),
        }
    }
}

/// One in-flight request from the HTTP task to the event loop.
pub(crate) struct A2aRequest {
    pub op: A2aOp,
    pub response: oneshot::Sender<Result<Value, RpcError>>,
}

/// Whether `method` sends the peer a message: `SendMessage` and its streaming twin share one
/// meaning. The method matches of [`parse_op`] and `served::classify` name both by hand.
pub(crate) fn is_send_message(method: &str) -> bool {
    matches!(method, "SendMessage" | "SendStreamingMessage")
}

pub(crate) fn task_id_param(params: &Value) -> Result<TaskId, RpcError> {
    params["id"]
        .as_str()
        .and_then(TaskId::from_uuid_str)
        .ok_or_else(|| RpcError::invalid_params("params.id must be a task id (uuid)"))
}

pub(crate) fn message_param(params: &Value) -> Result<super::Message, RpcError> {
    serde_json::from_value(params["message"].clone()).map_err(|error| {
        RpcError::invalid_params(&format!("params.message is not an A2A Message: {error}"))
    })
}

/// Map a JSON-RPC `method` + `params` (+ the URL-derived target) to an op.
///
/// # Errors
/// Unknown method (`-32601`) or malformed params (`-32602`).
pub(crate) fn parse_op(
    method: &str,
    params: &Value,
    target: Option<PeerRef>,
) -> Result<A2aOp, RpcError> {
    match method {
        // `SendStreamingMessage` shares `SendMessage`'s create semantics; the
        // streaming events ride the worker's pushed status/artifact frames (the
        // gossip push plane). The SSE `text/event-stream` edge encoding for an
        // off-the-shelf HTTP client is layered on top of that plane.
        "SendMessage" | "SendStreamingMessage" => Ok(A2aOp::SendMessage {
            to: target,
            message: message_param(params)?,
        }),
        // Subscribe returns the current snapshot; the caller (a party) keeps
        // receiving the worker's pushed frames.
        "GetTask" | "SubscribeToTask" => Ok(A2aOp::GetTask {
            task_id: task_id_param(params)?,
        }),
        "ListTasks" => Ok(A2aOp::ListTasks { peer: None }),
        "CancelTask" => Ok(A2aOp::CancelTask {
            task_id: task_id_param(params)?,
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
        other => Err(RpcError::method_not_found(other)),
    }
}

/// The `ListTasks` payload. `peer` scopes the listing to that counterparty's
/// tasks — what a remote caller may see; `None` lists everything, for the
/// local binding whose caller owns the daemon.
fn list_tasks(app: &A2aApp, peer: Option<&AuthorKey>, mesh: &MeshId) -> Value {
    // Sort by task id so the response order is stable across calls
    // (`app.tasks` is a HashMap with unspecified iteration order).
    let mut entries: Vec<(&TaskId, &TaskRecord)> = app
        .tasks
        .iter()
        .filter(|(_, rec)| peer.is_none_or(|peer| rec.peer_key == *peer))
        .collect();
    entries.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
    let tasks: Vec<Value> = entries
        .into_iter()
        .map(|(task_id, rec)| task_object(task_id, rec, mesh))
        .collect();
    serde_json::json!({ "tasks": tasks })
}

/// The A2A `Task` object for a live record — what `tasks/get`/`tasks/list`
/// return and what a task-creating `message/send` echoes. History/artifact
/// accumulation is not stored daemon-side (the event stream carries them);
/// mesh-specific coordinates ride `metadata`.
pub(crate) fn task_object(task_id: &TaskId, rec: &TaskRecord, mesh: &MeshId) -> Value {
    let role = |value: TaskRole| match value {
        TaskRole::Initiator => "initiator",
        TaskRole::Receiver => "worker",
    };
    let mut metadata = serde_json::json!({
        "mesh:peer": rec.peer.as_str(),
        "mesh:role": role(rec.role),
        "mesh:ball": role(rec.ball()),
        "mesh:review": rec.review,
        META_SEQ: rec.seq,
    });
    if rec.waiting_user {
        metadata[META_WAITING_USER] = Value::from(true);
    }
    if let Some(label) = &rec.label {
        metadata[META_LABEL] = Value::from(label.as_str());
    }
    if let Some(text) = &rec.status_text {
        metadata[META_STATUS_TEXT] = Value::from(text.as_str());
        if rec.status_truncated {
            metadata[META_STATUS_TRUNCATED] = Value::from(true);
        }
    }
    if let Some(progress) = rec.shown_progress {
        metadata[META_PROGRESS] = Value::from(progress.percent());
    }
    serde_json::json!({
        "id": task_id.as_str(),
        "contextId": mesh.as_str(),
        "status": { "state": rec.state },
        "metadata": metadata,
    })
}

/// The task that answers an opening call whose reply did not come before the deadline. The
/// record exists from dispatch, and the worker may hold the task already, so the caller gets the
/// task with `mesh:unconfirmed` and follows it like any other.
pub(crate) fn unconfirmed_task(task_id: &TaskId, rec: &TaskRecord, mesh: &MeshId) -> Value {
    let mut task = task_object(task_id, rec, mesh);
    task["metadata"][META_UNCONFIRMED] = Value::from(true);
    task
}

/// Why `task_id` cannot be canceled, or `None` when it can: a task that this node does not hold
/// is not found, and a task that is over, in the registry or already reaped, says that it is
/// over.
pub(crate) fn cancel_refusal(app: &A2aApp, task_id: &TaskId) -> Option<RpcError> {
    match (app.tasks.get(task_id), app.closed_tasks.get(task_id)) {
        (Some(rec), _) if rec.state.is_terminal() => {
            Some(RpcError::task_not_cancelable(task_id, rec.state))
        }
        (Some(_), _) => None,
        (None, Some(closed)) => Some(RpcError::task_not_cancelable(task_id, closed.state)),
        (None, None) => Some(RpcError::task_not_found(task_id)),
    }
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
                "metadata": { "mesh:peer": closed.peer.as_str(), META_SEQ: closed.seq },
            })
        })
        .ok_or_else(|| RpcError::task_not_found(task_id))
}

/// The error of a card route whose nickname names no one key of the roster: no live key uses it, or
/// two or more do.
pub(crate) fn address_error(nickname: &Nickname, error: KeyOfError) -> RpcError {
    RpcError {
        code: -32004,
        message: format!("cannot address '{nickname}': {error}"),
    }
}

/// Execute one op against the live loop state — the JSON-RPC binding's
/// dispatch, sharing the exact broadcast paths the IPC socket uses so the
/// two bindings cannot drift.
pub(crate) async fn handle_op(
    op: A2aOp,
    app: &mut A2aApp,
    state: &mut EventLoopState,
    context: &HandlerCtx<'_>,
) -> Result<Value, RpcError> {
    let HandlerCtx {
        mesh,
        author,
        sender,
        ..
    } = *context;
    // A clone, so the render sink does not alias the `&mut app` passed alongside it.
    let output = &app.output.clone();
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
            if let Some(refusal) = cancel_refusal(app, &task_id) {
                return Err(refusal);
            }
            let (_echo, pending) = emit_task_status(
                crate::a2a::broadcast_params::TaskStatusParams {
                    mesh,
                    author,
                    task_id: &task_id,
                    leg: super::WorkerLeg::canceled("canceled"),
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
        A2aOp::ChannelGet { channel } => Ok(state.view(channel)),
        A2aOp::ChannelMerge { channel, merge } => {
            broadcast_state_merge(
                state,
                StateMergeParams::new(MergeCtx::new(mesh, author, sender, output), channel, merge),
            )
            .await
            .map_err(|error| {
                let error = anyhow::Error::from(error);
                StateRefusalReply::of(&error).map_or_else(
                    || RpcError::internal(&error.to_string()),
                    |reply| RpcError::invalid_params(&reply.to_rpc_message()),
                )
            })?;
            Ok(serde_json::json!({ "ok": true }))
        }
        A2aOp::OwnCard => {
            // Served over the localhost binding, so the card names the JSONRPC
            // interface of this daemon alongside the always-present gossip one.
            // Only the localhost binding parses a card op, so the port never
            // reaches a gossip caller.
            let own_url = format!("http://127.0.0.1:{}/", app.a2a_port());
            let seal = state.identity().seal_public();
            let card = super::card::peer_card(context.our_pubkey, author, Some(&seal), &own_url);
            serde_json::to_value(&card).map_err(|error| RpcError::internal(&error.to_string()))
        }
        A2aOp::PeerCard { peer } => {
            // A nickname is a label: the card is the one of the key that a send to it would go to.
            let (key, label) = match &peer {
                PeerRef::Key(named) => (*named, named.to_string()),
                PeerRef::Nickname(nickname) => {
                    let key = state
                        .addressee_key(nickname)
                        .map_err(|error| address_error(nickname, error))?;
                    (key, nickname.to_string())
                }
            };
            // The card is built here from the roster, with nothing taken from the peer. The seal
            // key comes from the signed `PeerInfo` of the peer, which the engine verified.
            let Some(nickname) = state.known_nickname_of(&key) else {
                return Err(RpcError {
                    code: -32004,
                    message: format!("no member known as '{label}'"),
                });
            };
            // The route names the exact key, because two members can share a nickname.
            let relay_url = format!("http://127.0.0.1:{}/peers/{key}", app.a2a_port());
            let card = super::card::peer_card(key, &nickname, state.seal_key_of(&key), &relay_url);
            let card = serde_json::to_value(&card)
                .map_err(|error| RpcError::internal(&error.to_string()))?;
            Ok(card)
        }
    }
}

/// The addressee (must be `None`), payload, and identity of one
/// `message/send` RPC op — grouped so `send_message` stays within the
/// argument budget alongside its state/sender/output handles.
struct SendMessageParams<'caller> {
    to: Option<PeerRef>,
    message: super::Message,
    mesh: &'caller MeshId,
    author: &'caller Nickname,
}

/// A **broadcast** `message/send` (no addressee): re-author the wire payload
/// from the client Message's text projection (role/context/extension stamping
/// stays the daemon's job) and flood it. A *directed* `message/send` (task
/// creation) never reaches here — the localhost dispatch (`handle_http`)
/// intercepts it and routes it through the gossip request/response waiter, and
/// the gossip serve path ingests it directly — so a `Some(to)` here is a bug.
async fn send_message(
    params: SendMessageParams<'_>,
    state: &mut EventLoopState,
    sender: &MeshSender,
    output: &emission::Output,
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
    let body = MessageBody::try_new(&text)
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
    // A2A v1.0 `SendMessage` returns a oneof keyed by member name; a broadcast
    // has no task, so echo the daemon-authored Message as `{"message": …}`.
    let echo: super::Message = serde_json::from_str(frame.body.as_str())
        .map_err(|error| RpcError::internal(&error.to_string()))?;
    Ok(serde_json::json!({ "message": echo }))
}

#[cfg(test)]
mod cancel_tests {
    use std::time::Instant;

    use habilis_network::protocol::{AuthorKey, Nickname};

    use crate::a2a::a2a_app::A2aApp;
    use crate::a2a::task_record::{ClosedTask, TaskRecord, TaskRole};
    use crate::a2a::{TaskId, TaskState};

    fn key() -> AuthorKey {
        AuthorKey::from([0xb0; 32])
    }

    fn record(state: TaskState) -> TaskRecord {
        TaskRecord::open(
            (key(), Nickname::from("bob")),
            TaskRole::Receiver,
            state,
            Instant::now(),
        )
    }

    /// A task that is over, or already reaped, says that it is over. Only a task that this node
    /// does not hold is not found.
    #[test]
    fn a_task_that_is_over_answers_that_it_is_over() {
        let mut app = A2aApp::new();
        let live = TaskId::random();
        let done = TaskId::random();
        let reaped = TaskId::random();
        app.tasks.insert(live.clone(), record(TaskState::Working));
        app.tasks.insert(done.clone(), record(TaskState::Completed));
        app.closed_tasks.insert(
            reaped.clone(),
            ClosedTask {
                peer_key: key(),
                peer: Nickname::from("bob"),
                state: TaskState::Failed,
                seq: 3,
                owes_end: false,
            },
        );

        assert!(
            super::cancel_refusal(&app, &live).is_none(),
            "a live task can be canceled"
        );
        for over in [&done, &reaped] {
            let refusal =
                super::cancel_refusal(&app, over).expect("a task that is over is refused");
            assert_eq!(refusal.code, -32002, "the refusal is not-cancelable");
            assert!(
                refusal.message.contains("already"),
                "the answer says that the task is over, got {}",
                refusal.message
            );
        }
        let unknown =
            super::cancel_refusal(&app, &TaskId::random()).expect("an unknown task is refused");
        assert_eq!(unknown.code, -32001, "an unknown task is not found");
    }
}
