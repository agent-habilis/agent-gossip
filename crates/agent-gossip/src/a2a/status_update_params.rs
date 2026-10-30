use std::sync::atomic::AtomicU64;

use habilis_network::protocol::{MeshId, Message as Frame, MessageBody};

use super::extensions::{META_BELL, META_LAST_POLL, META_ORDER_MS, META_WAITING};
use super::frame_payload_error::FramePayloadError;
use super::{
    EXT_MESH_BROADCAST, META_BEAT, META_DONE, META_LABEL, META_TOTAL, Message, Part, Role,
    TaskArtifactUpdate, TaskId, TaskState, TaskStatus, TaskStatusUpdate, wire,
};

/// Compose a broadcast chat payload.
///
/// It sets `role: user` (chat is a client-side
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

/// Compose a direct chat payload: the same shape as [`compose_broadcast`] minus the broadcast extension.
///
/// A msg claims no extension because point-to-point *is*
/// plain A2A messaging; it is the broadcast that has something to declare. The
/// absent extension is also what [`msg_payload`] checks, so a msg cannot be
/// replayed into the broadcast validator.
#[must_use]
pub fn compose_msg(mesh: &MeshId, text: &str) -> Message {
    let mut message = Message::text(Role::User, text);
    message.context_id = Some(mesh.as_str().to_string());
    message
}

/// Compose the A2A `Message` that a directed `message/send` carries.
///
/// It is the task-creating brief (no `taskId`) or a follow-up into an existing task
/// (`taskId` set). `role: user` (a client submission), `contextId` = the
/// mesh; **no** broadcast extension (it is point-to-point).
/// `label` rides `metadata` as [`META_LABEL`] so the worker names the task the
/// same way the initiator does.
#[must_use]
pub fn send_message_payload(
    mesh: &MeshId,
    task_id: Option<&TaskId>,
    text: &str,
    label: Option<&str>,
) -> Message {
    send_message_payload_parts(mesh, task_id, vec![Part::text(text)], label)
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
    label: Option<&str>,
) -> Message {
    let mut message = Message::text(Role::User, "");
    message.parts = parts;
    message.context_id = Some(mesh.as_str().to_string());
    message.task_id = task_id.cloned();
    // Left None without a label so an unlabeled message stays byte-identical
    // on the wire (`metadata` is skip_serializing_if).
    message.metadata = label.map(|label| serde_json::json!({ META_LABEL: label }));
    message
}

/// Serialize any A2A payload (Message / status / artifact update) into a
/// frame body. Compact JSON never contains raw control characters (serde
/// escapes them), so this cannot fail `MessageBody`'s control-character
/// validation — only a pathological serialize error surfaces.
///
/// # Errors
/// A `serde_json` serialization failure.
pub(crate) fn payload_body<T: serde::Serialize>(
    payload: &T,
) -> Result<MessageBody, FramePayloadError> {
    let json = serde_json::to_string(payload).map_err(|source| FramePayloadError::Invalid {
        what: "failed to serialize a2a payload",
        source,
    })?;
    MessageBody::try_new(&json).map_err(|error| FramePayloadError::Body(error.to_string()))
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
fn chat_common(frame: &Frame) -> Result<Message, FramePayloadError> {
    let payload: Message =
        serde_json::from_str(frame.body.as_str()).map_err(|source| FramePayloadError::Invalid {
            what: "invalid a2a message payload",
            source,
        })?;
    if payload.message_id.as_str() != frame.id.as_str() {
        return Err(FramePayloadError::Shape(
            "a2a messageId does not match the frame id",
        ));
    }
    if payload.context_id.as_deref() != Some(frame.mesh.as_str()) {
        return Err(FramePayloadError::Shape(
            "a2a contextId does not name the frame's gossip",
        ));
    }
    if payload.role != Role::User {
        return Err(FramePayloadError::Shape(
            "chat carries role user; got agent",
        ));
    }
    Ok(payload)
}

/// Parse + validate the payload of a logical `a2a_broadcast` frame: the shared
/// checks plus the `mesh-broadcast` extension every broadcast declares.
///
/// # Errors
/// A payload that fails to parse or any frame/payload mismatch above.
pub fn broadcast_payload(frame: &Frame) -> Result<Message, FramePayloadError> {
    if !frame.kind.is_app(wire::BROADCAST) {
        return Err(FramePayloadError::WrongKind("a2a_broadcast"));
    }
    let payload = chat_common(frame)?;
    if !payload
        .extensions
        .iter()
        .any(|uri| uri == EXT_MESH_BROADCAST)
    {
        return Err(FramePayloadError::Shape(
            "a2a_broadcast without the mesh-broadcast extension",
        ));
    }
    Ok(payload)
}

/// Parse and validate the payload of a logical `a2a_msg` frame (a direct message).
///
/// Two checks beyond the shared ones, both about keeping the msg
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
pub fn msg_payload(frame: &Frame) -> Result<Message, FramePayloadError> {
    if !frame.kind.is_app(wire::MSG) {
        return Err(FramePayloadError::WrongKind("a2a_msg"));
    }
    let payload = chat_common(frame)?;
    if payload
        .extensions
        .iter()
        .any(|uri| uri == EXT_MESH_BROADCAST)
    {
        return Err(FramePayloadError::Shape(
            "a2a_msg carrying the mesh-broadcast extension",
        ));
    }
    if payload.task_id.is_some() {
        return Err(FramePayloadError::Shape(
            "a2a_msg carrying a taskId is a task leg, not a msg",
        ));
    }
    Ok(payload)
}

/// The text projection of a chat frame's payload.
///
/// An api-consumer convenience for reading a received frame without unpacking the A2A object.
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

/// The plain-text projection of a payload for display surfaces.
///
/// The surfaces are the operator line, the `display` string and the `body` convenience field. Text parts
/// joined by newline, non-text parts as bracketed placeholders.
#[must_use]
pub fn display_text(message: &Message) -> String {
    parts_text(&message.parts)
}

/// The plain-text projection of a run of parts: each part's [`Part::display`],
/// newline-joined. Shared by the message and artifact rendering paths.
#[must_use]
pub(crate) fn parts_text(parts: &[Part]) -> String {
    let lines: Vec<_> = parts.iter().map(Part::display).collect();
    lines.join("\n")
}

/// A task status update's task/state/note/metadata — the fields
/// [`status_update`] composes alongside the frame's mesh, grouped so the
/// call stays within the argument budget.
#[derive(Debug)]
#[non_exhaustive]
pub struct StatusUpdateParams<'caller> {
    /// The task that moves.
    pub task_id: &'caller TaskId,
    /// The state the task enters; a terminal state closes the stream.
    pub state: TaskState,
    /// Becomes `status.message` as an agent-role message. `None` sends no message.
    pub note: Option<&'caller str>,
    /// Opaque `metadata` on the update, for example from [`beat_metadata`].
    pub metadata: Option<serde_json::Value>,
}

impl<'caller> StatusUpdateParams<'caller> {
    /// An update that moves `task_id` to `state`, with no note and no metadata.
    #[must_use]
    pub fn new(task_id: &'caller TaskId, state: TaskState) -> Self {
        Self {
            task_id,
            state,
            note: None,
            metadata: None,
        }
    }

    /// Send `note` as the status message.
    #[must_use]
    pub fn with_note(mut self, note: &'caller str) -> Self {
        self.note = Some(note);
        self
    }

    /// Put `metadata` on the update.
    #[must_use]
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

/// Compose a task status update.
///
/// `note` becomes `status.message` (an
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
        let mut message = Message::text(Role::Agent, text);
        message.context_id = Some(mesh.as_str().to_string());
        message.task_id = Some(task_id.clone());
        Box::new(message)
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

/// Whether the local agent of a worker is reading: the answer a beat carries so
/// the initiator can tell a wedged agent from a live daemon without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReaderBeat {
    /// `true` when the agent armed a long poll, so a new message wakes it.
    pub bell: bool,
    /// Seconds since the agent last polled.
    pub last_poll_secs: u64,
}

/// As [`beat_metadata`], plus the worker's [`ReaderBeat`] when it has polled.
#[must_use]
pub fn beat_metadata_with_reader(
    fraction: Option<(u64, u64)>,
    reader: Option<ReaderBeat>,
) -> serde_json::Value {
    let mut meta = beat_metadata(fraction);
    if let (Some(reader), Some(map)) = (reader, meta.as_object_mut()) {
        map.insert(META_BELL.to_string(), reader.bell.into());
        map.insert(META_LAST_POLL.to_string(), reader.last_poll_secs.into());
    }
    meta
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
    parts: impl IntoIterator<Item = Part>,
) -> TaskArtifactUpdate {
    TaskArtifactUpdate {
        task_id: task_id.clone(),
        context_id: mesh.as_str().to_string(),
        artifact: super::Artifact {
            artifact_id: uuid::Uuid::new_v4().to_string(),
            parts: parts.into_iter().collect(),
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
pub fn status_payload(frame: &Frame) -> Result<TaskStatusUpdate, FramePayloadError> {
    if !frame.kind.is_app(wire::STATUS) {
        return Err(FramePayloadError::WrongKind("a2a_status"));
    }
    let payload: TaskStatusUpdate =
        serde_json::from_str(frame.body.as_str()).map_err(|source| FramePayloadError::Invalid {
            what: "invalid a2a status payload",
            source,
        })?;
    if payload.context_id != frame.mesh.as_str() {
        return Err(FramePayloadError::Shape(
            "a2a status contextId does not name the frame's gossip",
        ));
    }
    Ok(payload)
}

/// Parse + validate the artifact payload of a logical `A2aArtifact` frame.
///
/// # Errors
/// A payload that fails to parse or contradicts its frame.
pub fn artifact_payload(frame: &Frame) -> Result<TaskArtifactUpdate, FramePayloadError> {
    if !frame.kind.is_app(wire::ARTIFACT) {
        return Err(FramePayloadError::WrongKind("a2a_artifact"));
    }
    let payload: TaskArtifactUpdate =
        serde_json::from_str(frame.body.as_str()).map_err(|source| FramePayloadError::Invalid {
            what: "invalid a2a artifact payload",
            source,
        })?;
    if payload.context_id != frame.mesh.as_str() {
        return Err(FramePayloadError::Shape(
            "a2a artifact contextId does not name the frame's gossip",
        ));
    }
    Ok(payload)
}

/// Is this status update a liveness **beat** (keepalive/progress plumbing)
/// rather than a state transition? Wire-static: the `mesh:beat` marker.
#[must_use]
pub fn is_beat(update: &TaskStatusUpdate) -> bool {
    let beat = update
        .metadata
        .as_ref()
        .and_then(|meta| meta.get(META_BEAT));
    beat.and_then(serde_json::Value::as_bool).unwrap_or(false)
}

/// The [`ReaderBeat`] a beat carries. `None` unless both keys are present, so a
/// beat from an old peer, or from a worker that never polled, reads as unknown.
#[must_use]
pub fn beat_reader(update: &TaskStatusUpdate) -> Option<ReaderBeat> {
    let meta = update.metadata.as_ref()?;
    Some(ReaderBeat {
        bell: meta.get(META_BELL)?.as_bool()?,
        last_poll_secs: meta.get(META_LAST_POLL)?.as_u64()?,
    })
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

/// The initiator's one-line task label (`mesh:label`), when the brief carries one.
///
/// Peer-controlled text bound for a todo subject, so control characters
/// (a `\n` would forge a second row) are stripped and the length capped.
#[must_use]
pub fn task_label(message: &Message) -> Option<String> {
    let label: &str = message.metadata.as_ref()?.get(META_LABEL)?.as_str()?;
    let visible = label.chars().filter(|ch| !ch.is_control());
    let raw: String = visible.take(super::tuning::TASK_LABEL_MAX_CHARS).collect();
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// The `mesh:waiting` metadata of a `working` status leg, when a reason is
/// given. The counterpart of [`status_waiting`].
#[must_use]
pub fn waiting_metadata(reason: Option<&str>) -> Option<serde_json::Value> {
    reason.map(|reason| serde_json::json!({ META_WAITING: reason }))
}

/// The metadata of a status leg: its order value, and the `mesh:waiting` reason
/// when it has one. The counterpart of [`status_order`] and [`status_waiting`].
#[must_use]
pub fn status_metadata(waiting: Option<&str>, order_ms: u64) -> serde_json::Value {
    let mut meta = serde_json::json!({ META_ORDER_MS: order_ms });
    if let Some(reason) = waiting {
        meta[META_WAITING] = reason.into();
    }
    meta
}

/// The order value a status leg carries, when the peer sends one.
#[must_use]
pub fn status_order(update: &TaskStatusUpdate) -> Option<u64> {
    update.metadata.as_ref()?.get(META_ORDER_MS)?.as_u64()
}

/// The worker's clock now, in milliseconds: the order value of a status leg.
#[must_use]
pub fn leg_order_ms() -> u64 {
    next_order_ms(wall_clock_ms(), &LAST_ORDER_MS)
}

/// The last order value this process handed out. The wall clock can step back,
/// so the next value is kept above it; a restart resets it, and the clock then
/// carries the order on.
static LAST_ORDER_MS: AtomicU64 = AtomicU64::new(0);

/// The order value for a leg sent at `now_ms`: the clock, unless that is not
/// above the last value handed out, then one more than the last.
fn next_order_ms(now_ms: u64, last: &AtomicU64) -> u64 {
    use std::sync::atomic::Ordering::Relaxed;
    let previous = last
        .fetch_update(Relaxed, Relaxed, |last| {
            Some(now_ms.max(last.saturating_add(1)))
        })
        .unwrap_or_default();
    now_ms.max(previous.saturating_add(1))
}

fn wall_clock_ms() -> u64 {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(since_epoch.as_millis()).unwrap_or(u64::MAX)
}

/// The reason a worker gives for waiting on its own user (`mesh:waiting`), when the status leg carries one.
///
/// Peer-controlled text that `GetTask` echoes, so it
/// gets the same control-character strip and length cap as [`task_label`].
#[must_use]
pub fn status_waiting(update: &TaskStatusUpdate) -> Option<String> {
    let waiting: &str = update.metadata.as_ref()?.get(META_WAITING)?.as_str()?;
    let visible = waiting.chars().filter(|ch| !ch.is_control());
    let raw: String = visible.take(super::tuning::TASK_LABEL_MAX_CHARS).collect();
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// The task a **logical** frame belongs to, if any.
///
/// It is a worker-pushed status or
/// artifact frame (the frame correlation field). Neither chat tag is ever a
/// task — a payload carrying a `taskId` is refused by [`msg_payload`] — and task
/// `message/send` legs ride `A2aReq`/`A2aResp` (plumbing), so `None` for
/// everything else.
#[must_use]
pub fn frame_task_id(frame: &Frame) -> Option<TaskId> {
    // `task_id` is no longer an envelope field (8.0); it rides inside the body it
    // was always duplicated into, so we read it from there.
    let body: &str = frame.body.as_str();
    match frame.kind.app_tag()?.as_str() {
        wire::STATUS => serde_json::from_str::<TaskStatusUpdate>(body)
            .ok()
            .map(|payload| payload.task_id),
        wire::ARTIFACT => serde_json::from_str::<TaskArtifactUpdate>(body)
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
    let body: &str = frame.body.as_str();
    match frame
        .kind
        .app_tag()
        .map(habilis_network::protocol::AppTag::as_str)
    {
        Some(wire::STATUS) => {
            let payload: Option<TaskStatusUpdate> = serde_json::from_str(body).ok();
            payload
                .and_then(|payload| payload.status.message.map(|message| display_text(&message)))
                .unwrap_or_default()
        }
        Some(wire::ARTIFACT) => {
            let payload: Option<TaskArtifactUpdate> = serde_json::from_str(body).ok();
            payload
                .map(|payload| parts_text(&payload.artifact.parts))
                .unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// A well-formed task-status frame from `author`, for tests across modules.
#[cfg(test)]
pub(crate) fn test_status_frame(
    mesh: &MeshId,
    state: TaskState,
    to: Option<habilis_network::protocol::Nickname>,
) -> Frame {
    let status = status_update(mesh, StatusUpdateParams::new(&TaskId::random(), state));
    Frame::new_app(
        mesh,
        &habilis_network::protocol::Nickname::from("author"),
        habilis_network::protocol::AppFrameParams::new(
            wire::app_tag(wire::STATUS),
            to,
            None,
            payload_body(&status).expect("status body"),
        ),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;

    use habilis_network::protocol::{MeshId, MessageId, MessageKind};

    use super::{
        Frame, StatusUpdateParams, broadcast_payload, compose_broadcast, compose_msg, display_text,
        msg_payload, next_order_ms, payload_body, status_metadata, status_order, status_update,
        status_waiting, waiting_metadata,
    };
    use crate::a2a::wire;

    fn mesh() -> MeshId {
        MeshId::from("test")
    }

    /// A logical broadcast frame carrying `payload` — id already aligned to the
    /// payload's messageId, the invariant `broadcast_message` establishes.
    fn frame_for(payload: &super::Message) -> Frame {
        let body = payload_body(payload).expect("payload serializes");
        let mut frame = Frame::fixture(MessageKind::app_broadcast(wire::BROADCAST), body.as_str());
        frame.id = MessageId::try_new(payload.message_id.as_str()).expect("a2a id is a uuid");
        frame
    }

    /// The directed twin of [`frame_for`].
    fn msg_frame_for(payload: &super::Message) -> Frame {
        let body = payload_body(payload).expect("payload serializes");
        let mut frame = Frame::fixture(
            MessageKind::app_to(wire::MSG, "addressed-nick".into(), None),
            body.as_str(),
        );
        frame.id = MessageId::try_new(payload.message_id.as_str()).expect("a2a id is a uuid");
        frame
    }

    #[test]
    fn broadcast_round_trips() {
        let payload = compose_broadcast(&mesh(), "What is Rust?");
        let frame = frame_for(&payload);
        let parsed = broadcast_payload(&frame).expect("valid broadcast payload");
        assert_eq!(parsed, payload, "a payload round-trips");
        assert_eq!(
            display_text(&parsed),
            "What is Rust?",
            "the display text is the question"
        );
    }

    #[test]
    fn msg_round_trips() {
        let payload = compose_msg(&mesh(), "just between us");
        let frame = msg_frame_for(&payload);
        let parsed = msg_payload(&frame).expect("valid dm payload");
        assert_eq!(parsed, payload, "a payload round-trips between peers");
        assert_eq!(
            display_text(&parsed),
            "just between us",
            "the display text is kept between us"
        );
    }

    #[test]
    fn frame_id_mismatch_is_rejected() {
        let payload = compose_broadcast(&mesh(), "hi");
        let mut frame = frame_for(&payload);
        frame.id = "00000000-0000-0000-0000-0000000000ff".into();
        assert!(
            broadcast_payload(&frame).is_err(),
            "a broadcast with a malformed payload is refused"
        );
    }

    #[test]
    fn foreign_context_is_rejected() {
        let mut payload = compose_broadcast(&mesh(), "hi");
        payload.context_id = Some("other".to_string());
        let frame = frame_for(&payload);
        assert!(
            broadcast_payload(&frame).is_err(),
            "a broadcast frame without a payload is refused"
        );
    }

    #[test]
    fn missing_broadcast_extension_is_rejected() {
        let mut payload = compose_broadcast(&mesh(), "hi");
        payload.extensions.clear();
        let frame = frame_for(&payload);
        assert!(
            broadcast_payload(&frame).is_err(),
            "a broadcast of an unknown kind is refused"
        );
    }

    /// The two chat shapes must not be interchangeable: a broadcast payload
    /// under the directed tag is refused, and vice versa. Without this the
    /// `mesh-broadcast` extension would be decoration rather than a gate.
    #[test]
    fn chat_payloads_do_not_cross_tags() {
        let broadcast = compose_broadcast(&mesh(), "hi");
        assert!(
            msg_payload(&msg_frame_for(&broadcast)).is_err(),
            "a message frame for a broadcast is refused"
        );

        let dm = compose_msg(&mesh(), "hi");
        assert!(
            broadcast_payload(&frame_for(&dm)).is_err(),
            "a direct frame is not a broadcast payload"
        );
    }

    #[test]
    fn dm_carrying_a_task_id_is_rejected() {
        let mut payload = compose_msg(&mesh(), "hi");
        payload.task_id = Some(crate::a2a::TaskId::random());
        let frame = msg_frame_for(&payload);
        assert!(
            msg_payload(&frame).is_err(),
            "a message frame without a payload is refused"
        );
    }

    #[test]
    fn non_json_body_is_rejected() {
        let frame = Frame::fixture(MessageKind::app_broadcast(wire::BROADCAST), "plain text");
        assert!(
            broadcast_payload(&frame).is_err(),
            "a broadcast frame with no body is refused"
        );
    }

    /// The producer (`waiting_metadata`, used by `a2a status --waiting`) and the
    /// reader (`status_waiting`) agree, and a leg without a reason carries none.
    #[test]
    fn waiting_reason_round_trips_through_a_status_update() {
        let task_id = crate::a2a::TaskId::random();
        let update = |metadata| {
            status_update(
                &mesh(),
                StatusUpdateParams {
                    task_id: &task_id,
                    state: crate::a2a::TaskState::Working,
                    note: None,
                    metadata,
                },
            )
        };
        assert_eq!(
            status_waiting(&update(waiting_metadata(Some("asks its user")))).as_deref(),
            Some("asks its user"),
            "the status payload is the one sent"
        );
        assert_eq!(
            status_waiting(&update(waiting_metadata(None))),
            None,
            "a status without a waiting reason has none"
        );
    }

    /// A status leg carries its order value next to the reason, and the reader
    /// still finds the reason. A leg from a peer without the order value reads
    /// as none.
    #[test]
    fn a_status_leg_carries_its_order_value_and_its_reason() {
        let task_id = crate::a2a::TaskId::random();
        let update = |metadata| {
            status_update(
                &mesh(),
                StatusUpdateParams::new(&task_id, crate::a2a::TaskState::Working)
                    .with_metadata(metadata),
            )
        };
        let leg = update(status_metadata(Some("asks its user"), 7));
        assert_eq!(
            status_order(&leg),
            Some(7),
            "the status order is the sequence number"
        );
        assert_eq!(
            status_waiting(&leg).as_deref(),
            Some("asks its user"),
            "the waiting reason is the one given"
        );
        assert_eq!(
            status_order(&update(
                waiting_metadata(Some("asks its user")).expect("the test metadata is valid")
            )),
            None,
            "the status payload carries its fields"
        );
    }

    /// The wall clock can step back, so two legs of one worker must still get
    /// rising order values: the later leg keeps the higher one.
    #[test]
    fn a_clock_step_back_keeps_the_order_value_rising() {
        let last = AtomicU64::new(0);
        let first = next_order_ms(2_000, &last);
        let stepped_back = next_order_ms(1_500, &last);
        assert!(
            stepped_back > first,
            "a clock step back must not lower the order value: {stepped_back} <= {first}"
        );
    }
}
