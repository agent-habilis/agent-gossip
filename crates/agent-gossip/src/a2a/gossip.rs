use anyhow::{Context, Result, bail};

use agent_habilis_mesh::protocol::MeshId;
use agent_habilis_mesh::protocol::Message as Frame;
use agent_habilis_mesh::protocol::MessageBody;

use super::{
    EXT_MESH_BROADCAST, META_BEAT, META_DONE, META_TOTAL, Message, Part, Role, TaskArtifactUpdate,
    TaskId, TaskState, TaskStatus, TaskStatusUpdate, wire,
};

/// Compose a broadcast chat payload: `role: user` (chat is a client-side
/// submission), `contextId` = the mesh (the mesh *is* the conversation
/// context), and the `mesh-broadcast` extension — A2A messaging is
/// point-to-point, so a mesh-wide message declares itself.
#[must_use]
pub fn compose_broadcast(mesh: &MeshId, text: &str) -> Message {
    let mut message = Message::text(Role::User, text);
    message.context_id = Some(mesh.as_str().to_string());
    message.extensions = vec![EXT_MESH_BROADCAST.to_string()];
    message
}

/// Compose a direct chat payload — the same shape as [`compose_broadcast`] minus the
/// broadcast extension. A msg claims no extension because point-to-point *is*
/// plain A2A messaging; it is the broadcast that has something to declare. The
/// absent extension is also what [`msg_payload`] checks, so a msg cannot be
/// replayed into the broadcast validator.
#[must_use]
pub fn compose_msg(mesh: &MeshId, text: &str) -> Message {
    let mut message = Message::text(Role::User, text);
    message.context_id = Some(mesh.as_str().to_string());
    message
}

/// Compose the A2A `Message` a directed `message/send` carries — the
/// task-creating brief (no `taskId`) or a follow-up into an existing task
/// (`taskId` set). `role: user` (a client submission), `contextId` = the
/// mesh; **no** broadcast extension (it is point-to-point).
#[must_use]
pub fn send_message_payload(mesh: &MeshId, task_id: Option<&TaskId>, text: &str) -> Message {
    send_message_payload_parts(mesh, task_id, vec![Part::text(text)])
}

/// As [`send_message_payload`] but carrying arbitrary parts — used when the
/// message attaches a file (a `Part` with a blob `url`) alongside or instead of
/// text. Input files ride the request `Message.parts`; they are never wrapped in
/// an `Artifact` (the A2A request has no artifact slot).
#[must_use]
pub(crate) fn send_message_payload_parts(
    mesh: &MeshId,
    task_id: Option<&TaskId>,
    parts: Vec<Part>,
) -> Message {
    let mut message = Message::text(Role::User, "");
    message.parts = parts;
    message.context_id = Some(mesh.as_str().to_string());
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

/// The checks both chat tags share, on a **logical** frame (a single wire
/// message, or the reassembled view of a sharded body — never a raw shard).
/// The frame is transport, the payload is the A2A layer, and a mismatch is a
/// crafted message:
///
/// - `messageId` must equal the frame's logical id, so the id every consumer
///   sees (dedup, poll cursor, echo) *is* the A2A id.
/// - `contextId` must name the frame's mesh (the receive path already gates
///   the frame's mesh against ours).
/// - chat is `role: user` by construction (see [`compose_broadcast`]).
fn chat_common(frame: &Frame) -> Result<Message> {
    let payload: Message =
        serde_json::from_str(frame.body.as_str()).context("invalid a2a message payload")?;
    if payload.message_id.as_str() != frame.id.as_str() {
        bail!("a2a messageId does not match the frame id");
    }
    if payload.context_id.as_deref() != Some(frame.mesh.as_str()) {
        bail!("a2a contextId does not name the frame's gossip");
    }
    if payload.role != Role::User {
        bail!("chat carries role user; got agent");
    }
    Ok(payload)
}

/// Parse + validate the payload of a logical `a2a_broadcast` frame: the shared
/// checks plus the `mesh-broadcast` extension every broadcast declares.
///
/// # Errors
/// A payload that fails to parse or any frame/payload mismatch above.
pub fn broadcast_payload(frame: &Frame) -> Result<Message> {
    if !frame.kind.is_app(wire::BROADCAST) {
        bail!("not an a2a_broadcast frame");
    }
    let payload = chat_common(frame)?;
    if !payload
        .extensions
        .iter()
        .any(|uri| uri == EXT_MESH_BROADCAST)
    {
        bail!("a2a_broadcast without the mesh-broadcast extension");
    }
    Ok(payload)
}

/// Parse + validate the payload of a logical `a2a_msg` frame (a direct
/// message). Two checks beyond the shared ones, both about keeping the msg
/// distinct from its neighbours rather than about the msg itself:
///
/// - it must **not** declare `mesh-broadcast`. A msg carrying it is a broadcast
///   payload replayed under the directed tag, and refusing it here is what
///   stops the two chat shapes from being interchangeable on the wire.
/// - it must carry no `taskId`. That is the only thing separating a msg from a
///   task's `message/send` leg, which is a different lifecycle entirely.
///
/// # Errors
/// A payload that fails to parse or any frame/payload mismatch above.
pub fn msg_payload(frame: &Frame) -> Result<Message> {
    if !frame.kind.is_app(wire::MSG) {
        bail!("not an a2a_msg frame");
    }
    let payload = chat_common(frame)?;
    if payload
        .extensions
        .iter()
        .any(|uri| uri == EXT_MESH_BROADCAST)
    {
        bail!("a2a_msg carrying the mesh-broadcast extension");
    }
    if payload.task_id.is_some() {
        bail!("a2a_msg carrying a taskId is a task leg, not a msg");
    }
    Ok(payload)
}

/// The text projection of a chat frame's payload — the api-consumer
/// convenience for reading a received frame without unpacking the A2A object.
/// Covers both chat tags. `None` for a non-chat frame or an unparseable
/// payload.
#[must_use]
pub fn chat_text(frame: &Frame) -> Option<String> {
    if !frame.kind.is_app(wire::BROADCAST) && !frame.kind.is_app(wire::MSG) {
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
pub(crate) fn parts_text(parts: &[Part]) -> String {
    parts
        .iter()
        .map(Part::display)
        .collect::<Vec<_>>()
        .join("\n")
}

/// A task status update's task/state/note/metadata — the fields
/// [`status_update`] composes alongside the frame's mesh, grouped so the
/// call stays within the argument budget.
#[derive(Debug)]
pub struct StatusUpdateParams<'a> {
    pub task_id: &'a TaskId,
    pub state: TaskState,
    pub note: Option<&'a str>,
    pub metadata: Option<serde_json::Value>,
}

/// Compose a task status update. `note` becomes `status.message` (an
/// agent-role Message — a worker's acceptance note, a question, a result
/// summary). v1.0 dropped `TaskStatusUpdateEvent.final`; a terminal `state` is
/// itself the stream-close signal.
#[must_use]
pub fn status_update(mesh: &MeshId, params: StatusUpdateParams<'_>) -> TaskStatusUpdate {
    let StatusUpdateParams {
        task_id,
        state,
        note,
        metadata,
    } = params;
    let message = note.map(|text| {
        let mut msg = Message::text(Role::Agent, text);
        msg.context_id = Some(mesh.as_str().to_string());
        msg.task_id = Some(task_id.clone());
        Box::new(msg)
    });
    TaskStatusUpdate {
        task_id: task_id.clone(),
        context_id: mesh.as_str().to_string(),
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
pub fn artifact_update(mesh: &MeshId, task_id: &TaskId, text: &str) -> TaskArtifactUpdate {
    artifact_update_parts(mesh, task_id, vec![Part::text(text)])
}

/// As [`artifact_update`] but carrying arbitrary parts — used when the result
/// attaches a file (a `Part` with a blob `url`) alongside or instead of text.
#[must_use]
pub fn artifact_update_parts(
    mesh: &MeshId,
    task_id: &TaskId,
    parts: Vec<Part>,
) -> TaskArtifactUpdate {
    TaskArtifactUpdate {
        task_id: task_id.clone(),
        context_id: mesh.as_str().to_string(),
        artifact: super::Artifact {
            artifact_id: uuid::Uuid::new_v4().to_string(),
            parts,
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
    if !frame.kind.is_app(wire::STATUS) {
        bail!("not an a2a_status frame");
    }
    let payload: TaskStatusUpdate =
        serde_json::from_str(frame.body.as_str()).context("invalid a2a status payload")?;
    if payload.context_id != frame.mesh.as_str() {
        bail!("a2a status contextId does not name the frame's gossip");
    }
    Ok(payload)
}

/// Parse + validate the artifact payload of a logical `A2aArtifact` frame.
///
/// # Errors
/// A payload that fails to parse or contradicts its frame.
pub fn artifact_payload(frame: &Frame) -> Result<TaskArtifactUpdate> {
    if !frame.kind.is_app(wire::ARTIFACT) {
        bail!("not an a2a_artifact frame");
    }
    let payload: TaskArtifactUpdate =
        serde_json::from_str(frame.body.as_str()).context("invalid a2a artifact payload")?;
    if payload.context_id != frame.mesh.as_str() {
        bail!("a2a artifact contextId does not name the frame's gossip");
    }
    Ok(payload)
}

/// Is this status update a liveness **beat** (keepalive/progress plumbing)
/// rather than a state transition? Wire-static: the `mesh:beat` marker.
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
/// artifact frame (the frame correlation field). Neither chat tag is ever a
/// task — a payload carrying a `taskId` is refused by [`msg_payload`] — and task
/// `message/send` legs ride `A2aReq`/`A2aResp` (plumbing), so `None` for
/// everything else.
#[must_use]
pub fn frame_task_id(frame: &Frame) -> Option<TaskId> {
    // `task_id` is no longer an envelope field (8.0); it rides inside the body it
    // was always duplicated into, so we read it from there.
    match frame.kind.app_tag()?.as_str() {
        wire::STATUS => serde_json::from_str::<TaskStatusUpdate>(frame.body.as_str())
            .ok()
            .map(|payload| payload.task_id),
        wire::ARTIFACT => serde_json::from_str::<TaskArtifactUpdate>(frame.body.as_str())
            .ok()
            .map(|payload| payload.task_id),
        _ => None,
    }
}

/// The A2A event **kind** a worker-pushed task frame reads as — the
/// event-stream discriminator (`"status-update"` / `"artifact-update"`).
/// `None` for non-task-push kinds.
#[must_use]
pub fn task_event_kind(frame: &Frame) -> Option<&'static str> {
    match frame.kind.app_tag()?.as_str() {
        wire::STATUS => Some("status-update"),
        wire::ARTIFACT => Some("artifact-update"),
        _ => None,
    }
}

/// The task **state** a worker-pushed frame carries: the status payload's
/// state, or `input-required` for an artifact (the review park). `None` for
/// non-task-push kinds or an unparseable payload.
#[must_use]
pub fn frame_task_state(frame: &Frame) -> Option<TaskState> {
    match frame.kind.app_tag()?.as_str() {
        wire::STATUS => serde_json::from_str::<TaskStatusUpdate>(frame.body.as_str())
            .ok()
            .map(|payload| payload.status.state),
        wire::ARTIFACT => Some(TaskState::InputRequired),
        _ => None,
    }
}

/// The display/body text of a worker-pushed task frame: a status note's text,
/// or an artifact's parts.
#[must_use]
pub fn task_text(frame: &Frame) -> String {
    match frame
        .kind
        .app_tag()
        .map(agent_habilis_mesh::protocol::AppTag::as_str)
    {
        Some(wire::STATUS) => serde_json::from_str::<TaskStatusUpdate>(frame.body.as_str())
            .ok()
            .and_then(|payload| payload.status.message.map(|msg| display_text(&msg)))
            .unwrap_or_default(),
        Some(wire::ARTIFACT) => serde_json::from_str::<TaskArtifactUpdate>(frame.body.as_str())
            .ok()
            .map(|payload| parts_text(&payload.artifact.parts))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Frame, broadcast_payload, compose_broadcast, compose_msg, display_text, msg_payload,
        payload_body,
    };
    use agent_habilis_mesh::protocol::MeshId;
    use agent_habilis_mesh::protocol::MessageId;
    use agent_habilis_mesh::protocol::MessageKind;

    fn mesh() -> MeshId {
        MeshId::from("💬test")
    }

    /// A logical broadcast frame carrying `payload` — id already aligned to the
    /// payload's messageId, the invariant `broadcast_message` establishes.
    fn frame_for(payload: &super::Message) -> Frame {
        let body = payload_body(payload).expect("payload serializes");
        let mut frame = Frame::fixture(
            MessageKind::app_broadcast(crate::a2a::wire::BROADCAST),
            body.as_str(),
        );
        frame.id = MessageId::new(payload.message_id.as_str()).expect("a2a id is a uuid");
        frame
    }

    /// The directed twin of [`frame_for`].
    fn msg_frame_for(payload: &super::Message) -> Frame {
        let body = payload_body(payload).expect("payload serializes");
        let mut frame = Frame::fixture(
            MessageKind::app_to(crate::a2a::wire::MSG, "addressed-nick".into(), None),
            body.as_str(),
        );
        frame.id = MessageId::new(payload.message_id.as_str()).expect("a2a id is a uuid");
        frame
    }

    #[test]
    fn broadcast_round_trips() {
        let payload = compose_broadcast(&mesh(), "What is Rust?");
        let frame = frame_for(&payload);
        let parsed = broadcast_payload(&frame).expect("valid broadcast payload");
        assert_eq!(parsed, payload);
        assert_eq!(display_text(&parsed), "What is Rust?");
    }

    #[test]
    fn msg_round_trips() {
        let payload = compose_msg(&mesh(), "just between us");
        let frame = msg_frame_for(&payload);
        let parsed = msg_payload(&frame).expect("valid dm payload");
        assert_eq!(parsed, payload);
        assert_eq!(display_text(&parsed), "just between us");
    }

    #[test]
    fn frame_id_mismatch_is_rejected() {
        let payload = compose_broadcast(&mesh(), "hi");
        let mut frame = frame_for(&payload);
        frame.id = "00000000-0000-0000-0000-0000000000ff".into();
        assert!(broadcast_payload(&frame).is_err());
    }

    #[test]
    fn foreign_context_is_rejected() {
        let mut payload = compose_broadcast(&mesh(), "hi");
        payload.context_id = Some("💬other".to_string());
        let frame = frame_for(&payload);
        assert!(broadcast_payload(&frame).is_err());
    }

    #[test]
    fn missing_broadcast_extension_is_rejected() {
        let mut payload = compose_broadcast(&mesh(), "hi");
        payload.extensions.clear();
        let frame = frame_for(&payload);
        assert!(broadcast_payload(&frame).is_err());
    }

    /// The two chat shapes must not be interchangeable: a broadcast payload
    /// under the directed tag is refused, and vice versa. Without this the
    /// `mesh-broadcast` extension would be decoration rather than a gate.
    #[test]
    fn chat_payloads_do_not_cross_tags() {
        let broadcast = compose_broadcast(&mesh(), "hi");
        assert!(msg_payload(&msg_frame_for(&broadcast)).is_err());

        let dm = compose_msg(&mesh(), "hi");
        assert!(broadcast_payload(&frame_for(&dm)).is_err());
    }

    #[test]
    fn dm_carrying_a_task_id_is_rejected() {
        let mut payload = compose_msg(&mesh(), "hi");
        payload.task_id = Some(crate::a2a::TaskId::random());
        let frame = msg_frame_for(&payload);
        assert!(msg_payload(&frame).is_err());
    }

    #[test]
    fn non_json_body_is_rejected() {
        let frame = Frame::fixture(
            MessageKind::app_broadcast(crate::a2a::wire::BROADCAST),
            "plain text",
        );
        assert!(broadcast_payload(&frame).is_err());
    }
}
