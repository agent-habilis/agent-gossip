use anyhow::{Context, Result, bail};

use crate::protocol::Message as Frame;
use crate::protocol::swarm::SwarmId;
use crate::protocol::{MessageBody, MessageKind};

use super::{
    EXT_SWARM_BROADCAST, META_BEAT, META_DONE, META_TOTAL, Message, Part, Role, TaskArtifactUpdate,
    TaskId, TaskState, TaskStatus, TaskStatusUpdate,
};

/// Compose a broadcast chat payload: `role: user` (chat is a client-side
/// submission), `contextId` = the swarm (the swarm *is* the conversation
/// context), and the `swarm-broadcast` extension — A2A messaging is
/// point-to-point, so a swarm-wide message declares itself.
#[must_use]
pub fn chat_message(swarm: &SwarmId, text: &str) -> Message {
    let mut message = Message::text(Role::User, text);
    message.context_id = Some(swarm.as_str().to_string());
    message.extensions = vec![EXT_SWARM_BROADCAST.to_string()];
    message
}

/// Compose the A2A `Message` a directed `message/send` carries — the
/// task-creating brief (no `taskId`) or a follow-up into an existing task
/// (`taskId` set). `role: user` (a client submission), `contextId` = the
/// swarm; **no** broadcast extension (it is point-to-point).
#[must_use]
pub fn send_message_payload(swarm: &SwarmId, task_id: Option<&TaskId>, text: &str) -> Message {
    let mut message = Message::text(Role::User, text);
    message.context_id = Some(swarm.as_str().to_string());
    message.task_id = task_id.cloned();
    message
}

/// Serialize any A2A payload (Message / status / artifact update) into a
/// frame body. Compact JSON never contains raw control characters (serde
/// escapes them), so this cannot fail `MessageBody`'s control-character
/// validation — only a pathological serialize error surfaces.
///
/// # Errors
/// A `serde_json` serialization failure.
pub(crate) fn payload_body<T: serde::Serialize>(payload: &T) -> Result<MessageBody> {
    let json = serde_json::to_string(payload).context("failed to serialize a2a payload")?;
    MessageBody::new(json).map_err(|error| anyhow::anyhow!("{error}"))
}

/// Parse + validate the chat payload of a **logical** `A2aMsg` frame (a single
/// wire message, or the reassembled view of a sharded body — never a raw
/// shard). `A2aMsg` is swarm broadcast chat; the frame is transport, the
/// payload is the A2A layer, and a mismatch is a crafted message:
///
/// - `messageId` must equal the frame's logical id, so the id every consumer
///   sees (dedup, poll cursor, echo) *is* the A2A id.
/// - `contextId` must name the frame's swarm (the receive path already gates
///   the frame's swarm against ours).
/// - it must declare the `swarm-broadcast` extension (every `A2aMsg` is
///   broadcast).
/// - chat is `role: user` by construction (see [`chat_message`]).
///
/// # Errors
/// A payload that fails to parse or any frame/payload mismatch above.
pub fn chat_payload(frame: &Frame) -> Result<Message> {
    let MessageKind::A2aMsg = &frame.kind else {
        bail!("not an a2a_msg frame");
    };
    let payload: Message =
        serde_json::from_str(frame.body.as_str()).context("invalid a2a message payload")?;
    if payload.message_id.as_str() != frame.id.as_str() {
        bail!("a2a messageId does not match the frame id");
    }
    if payload.context_id.as_deref() != Some(frame.swarm.as_str()) {
        bail!("a2a contextId does not name the frame's swarm");
    }
    if !payload
        .extensions
        .iter()
        .any(|uri| uri == EXT_SWARM_BROADCAST)
    {
        bail!("a2a_msg without the swarm-broadcast extension");
    }
    if payload.role != Role::User {
        bail!("chat carries role user; got agent");
    }
    Ok(payload)
}

/// The text projection of a chat frame's payload — the embed-consumer
/// convenience for reading a received frame without unpacking the A2A object.
/// `None` for a non-chat frame or an unparseable payload.
#[must_use]
pub fn chat_text(frame: &Frame) -> Option<String> {
    if !matches!(frame.kind, MessageKind::A2aMsg) {
        return None;
    }
    serde_json::from_str::<Message>(frame.body.as_str())
        .ok()
        .map(|payload| display_text(&payload))
}

/// The plain-text projection of a payload for display surfaces (the operator
/// line, the `display` string, the `body` convenience field): text parts
/// joined by newline, non-text parts as bracketed placeholders.
#[must_use]
pub fn display_text(message: &Message) -> String {
    parts_text(&message.parts)
}

/// The plain-text projection of a run of parts: each part's [`Part::display`],
/// newline-joined. Shared by the message and artifact rendering paths.
#[must_use]
pub fn parts_text(parts: &[Part]) -> String {
    parts
        .iter()
        .map(Part::display)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Compose a task status update. `note` becomes `status.message` (an
/// agent-role Message — a worker's acceptance note, a question, a result
/// summary). v1.0 dropped `TaskStatusUpdateEvent.final`; a terminal `state` is
/// itself the stream-close signal.
#[must_use]
pub fn status_update(
    swarm: &SwarmId,
    task_id: &TaskId,
    state: TaskState,
    note: Option<&str>,
    metadata: Option<serde_json::Value>,
) -> TaskStatusUpdate {
    let message = note.map(|text| {
        let mut msg = Message::text(Role::Agent, text);
        msg.context_id = Some(swarm.as_str().to_string());
        msg.task_id = Some(task_id.clone());
        Box::new(msg)
    });
    TaskStatusUpdate {
        task_id: task_id.clone(),
        context_id: swarm.as_str().to_string(),
        status: TaskStatus {
            state,
            message,
            timestamp: None,
        },
        metadata,
    }
}

/// The beat metadata for a keepalive/progress status: the wire-static
/// plumbing marker plus an optional `done/total` fraction.
#[must_use]
pub fn beat_metadata(fraction: Option<(u64, u64)>) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert(META_BEAT.to_string(), serde_json::Value::Bool(true));
    if let Some((done, total)) = fraction {
        map.insert(META_DONE.to_string(), done.into());
        map.insert(META_TOTAL.to_string(), total.into());
    }
    serde_json::Value::Object(map)
}

/// Compose a task artifact update (the worker's result): one artifact whose
/// parts carry the result text.
#[must_use]
pub fn artifact_update(swarm: &SwarmId, task_id: &TaskId, text: &str) -> TaskArtifactUpdate {
    TaskArtifactUpdate {
        task_id: task_id.clone(),
        context_id: swarm.as_str().to_string(),
        artifact: super::Artifact {
            artifact_id: uuid::Uuid::new_v4().to_string(),
            parts: vec![Part::text(text)],
            name: None,
            description: None,
            extensions: Vec::new(),
            metadata: None,
        },
        append: None,
        last_chunk: Some(true),
        metadata: None,
    }
}

/// Parse + validate the status payload of a logical `A2aStatus` frame:
/// the payload's `taskId`/`contextId` must agree with the frame.
///
/// # Errors
/// A payload that fails to parse or contradicts its frame.
pub fn status_payload(frame: &Frame) -> Result<TaskStatusUpdate> {
    let MessageKind::A2aStatus { task_id, .. } = &frame.kind else {
        bail!("not an a2a_status frame");
    };
    let payload: TaskStatusUpdate =
        serde_json::from_str(frame.body.as_str()).context("invalid a2a status payload")?;
    if payload.task_id != *task_id {
        bail!("a2a status taskId does not match the frame");
    }
    if payload.context_id != frame.swarm.as_str() {
        bail!("a2a status contextId does not name the frame's swarm");
    }
    Ok(payload)
}

/// Parse + validate the artifact payload of a logical `A2aArtifact` frame.
///
/// # Errors
/// A payload that fails to parse or contradicts its frame.
pub fn artifact_payload(frame: &Frame) -> Result<TaskArtifactUpdate> {
    let MessageKind::A2aArtifact { task_id, .. } = &frame.kind else {
        bail!("not an a2a_artifact frame");
    };
    let payload: TaskArtifactUpdate =
        serde_json::from_str(frame.body.as_str()).context("invalid a2a artifact payload")?;
    if payload.task_id != *task_id {
        bail!("a2a artifact taskId does not match the frame");
    }
    if payload.context_id != frame.swarm.as_str() {
        bail!("a2a artifact contextId does not name the frame's swarm");
    }
    Ok(payload)
}

/// Is this status update a liveness **beat** (keepalive/progress plumbing)
/// rather than a state transition? Wire-static: the `swarm:beat` marker.
#[must_use]
pub fn is_beat(update: &TaskStatusUpdate) -> bool {
    update
        .metadata
        .as_ref()
        .and_then(|meta| meta.get(META_BEAT))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// The `done/total` fraction a beat carries, if any.
#[must_use]
pub fn beat_fraction(update: &TaskStatusUpdate) -> Option<(u64, u64)> {
    let meta = update.metadata.as_ref()?;
    Some((
        meta.get(META_DONE)?.as_u64()?,
        meta.get(META_TOTAL)?.as_u64()?,
    ))
}

/// The task a **logical** frame belongs to, if any: a worker-pushed status or
/// artifact frame (the frame correlation field). `A2aMsg` is broadcast chat —
/// never a task — and task `message/send` legs ride `A2aReq`/`A2aResp`
/// (plumbing), so `None` for everything else.
#[must_use]
pub fn frame_task_id(frame: &Frame) -> Option<TaskId> {
    match &frame.kind {
        MessageKind::A2aStatus { task_id, .. } | MessageKind::A2aArtifact { task_id, .. } => {
            Some(task_id.clone())
        }
        MessageKind::A2aMsg
        | MessageKind::Presence { .. }
        | MessageKind::PeerInfo
        | MessageKind::Digest
        | MessageKind::StateDigest
        | MessageKind::MetaDigest
        | MessageKind::Ping
        | MessageKind::Pong { .. }
        | MessageKind::A2aReq { .. }
        | MessageKind::A2aResp { .. }
        | MessageKind::State
        | MessageKind::Meta => None,
    }
}

/// The A2A event **kind** a worker-pushed task frame reads as — the
/// event-stream discriminator (`"status-update"` / `"artifact-update"`).
/// `None` for non-task-push kinds.
#[must_use]
pub fn task_event_kind(frame: &Frame) -> Option<&'static str> {
    match &frame.kind {
        MessageKind::A2aStatus { .. } => Some("status-update"),
        MessageKind::A2aArtifact { .. } => Some("artifact-update"),
        MessageKind::A2aMsg
        | MessageKind::Presence { .. }
        | MessageKind::PeerInfo
        | MessageKind::Digest
        | MessageKind::StateDigest
        | MessageKind::MetaDigest
        | MessageKind::Ping
        | MessageKind::Pong { .. }
        | MessageKind::A2aReq { .. }
        | MessageKind::A2aResp { .. }
        | MessageKind::State
        | MessageKind::Meta => None,
    }
}

/// The task **state** a worker-pushed frame carries: the status payload's
/// state, or `input-required` for an artifact (the review park). `None` for
/// non-task-push kinds or an unparseable payload.
#[must_use]
pub fn frame_task_state(frame: &Frame) -> Option<TaskState> {
    match &frame.kind {
        MessageKind::A2aStatus { .. } => {
            serde_json::from_str::<TaskStatusUpdate>(frame.body.as_str())
                .ok()
                .map(|payload| payload.status.state)
        }
        MessageKind::A2aArtifact { .. } => Some(TaskState::InputRequired),
        MessageKind::A2aMsg
        | MessageKind::Presence { .. }
        | MessageKind::PeerInfo
        | MessageKind::Digest
        | MessageKind::StateDigest
        | MessageKind::MetaDigest
        | MessageKind::Ping
        | MessageKind::Pong { .. }
        | MessageKind::A2aReq { .. }
        | MessageKind::A2aResp { .. }
        | MessageKind::State
        | MessageKind::Meta => None,
    }
}

/// The display/body text of a worker-pushed task frame: a status note's text,
/// or an artifact's parts.
#[must_use]
pub fn task_text(frame: &Frame) -> String {
    match &frame.kind {
        MessageKind::A2aStatus { .. } => {
            serde_json::from_str::<TaskStatusUpdate>(frame.body.as_str())
                .ok()
                .and_then(|payload| payload.status.message.map(|msg| display_text(&msg)))
                .unwrap_or_default()
        }
        MessageKind::A2aArtifact { .. } => {
            serde_json::from_str::<TaskArtifactUpdate>(frame.body.as_str())
                .ok()
                .map(|payload| parts_text(&payload.artifact.parts))
                .unwrap_or_default()
        }
        MessageKind::A2aMsg
        | MessageKind::Presence { .. }
        | MessageKind::PeerInfo
        | MessageKind::Digest
        | MessageKind::StateDigest
        | MessageKind::MetaDigest
        | MessageKind::Ping
        | MessageKind::Pong { .. }
        | MessageKind::A2aReq { .. }
        | MessageKind::A2aResp { .. }
        | MessageKind::State
        | MessageKind::Meta => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{Frame, chat_message, chat_payload, display_text, payload_body};
    use crate::protocol::MessageKind;
    use crate::protocol::message::MessageId;
    use crate::protocol::swarm::SwarmId;

    fn swarm() -> SwarmId {
        SwarmId::from("🐝test")
    }

    /// A logical broadcast frame carrying `payload` — id already aligned to the
    /// payload's messageId, the invariant `broadcast_message` establishes.
    fn frame_for(payload: &super::Message) -> Frame {
        let body = payload_body(payload).expect("payload serializes");
        let mut frame = Frame::fixture(MessageKind::A2aMsg, body.as_str());
        frame.id = MessageId::new(payload.message_id.as_str()).expect("a2a id is a uuid");
        frame
    }

    #[test]
    fn broadcast_round_trips() {
        let payload = chat_message(&swarm(), "What is Rust?");
        let frame = frame_for(&payload);
        let parsed = chat_payload(&frame).expect("valid broadcast payload");
        assert_eq!(parsed, payload);
        assert_eq!(display_text(&parsed), "What is Rust?");
    }

    #[test]
    fn frame_id_mismatch_is_rejected() {
        let payload = chat_message(&swarm(), "hi");
        let mut frame = frame_for(&payload);
        frame.id = "00000000-0000-0000-0000-0000000000ff".into();
        assert!(chat_payload(&frame).is_err());
    }

    #[test]
    fn foreign_context_is_rejected() {
        let mut payload = chat_message(&swarm(), "hi");
        payload.context_id = Some("🐝other".to_string());
        let frame = frame_for(&payload);
        assert!(chat_payload(&frame).is_err());
    }

    #[test]
    fn missing_broadcast_extension_is_rejected() {
        let mut payload = chat_message(&swarm(), "hi");
        payload.extensions.clear();
        let frame = frame_for(&payload);
        assert!(chat_payload(&frame).is_err());
    }

    #[test]
    fn non_json_body_is_rejected() {
        let frame = Frame::fixture(MessageKind::A2aMsg, "plain text");
        assert!(chat_payload(&frame).is_err());
    }
}
