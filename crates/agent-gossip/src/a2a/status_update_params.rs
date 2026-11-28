use habilis_network::protocol::{MeshId, Message as Frame, MessageBody};

use super::extensions::{META_CHECKIN_AGE, META_SEQ, META_WAITING_USER};
use super::frame_payload_error::FramePayloadError;
use super::{
    EXT_MESH_BROADCAST, META_BEAT, META_LABEL, META_PROGRESS, Message, Part, Role,
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
    let mut message = Message::text(Role::User, text);
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
/// plumbing marker plus the last progress, when there is one.
#[must_use]
pub fn beat_metadata(progress: Option<Progress>) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert(META_BEAT.to_string(), serde_json::Value::Bool(true));
    if let Some(progress) = progress {
        map.insert(META_PROGRESS.to_string(), progress.percent().into());
    }
    serde_json::Value::Object(map)
}

/// What a worker adds to a beat beyond the plumbing marker.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct BeatExtras {
    /// The last progress reported, replayed so a lost check-in costs nothing.
    pub progress: Option<Progress>,
    /// Seconds since the worker's agent last checked in on this task. The sender takes the
    /// smaller of it and the silence it measured itself.
    pub checkin_age_secs: Option<u64>,
    /// The seq of the worker's last leg, so that the sender can tell a leg it never got. A beat
    /// does not take a seq of its own.
    pub seq: Option<u64>,
}

/// As [`beat_metadata`], plus the worker's check-in age and seq.
#[must_use]
pub fn beat_metadata_with_extras(extras: BeatExtras) -> serde_json::Value {
    let mut meta = beat_metadata(extras.progress);
    if let Some(map) = meta.as_object_mut() {
        if let Some(age) = extras.checkin_age_secs {
            map.insert(META_CHECKIN_AGE.to_string(), age.into());
        }
        if let Some(seq) = extras.seq {
            map.insert(META_SEQ.to_string(), seq.into());
        }
    }
    meta
}

/// Compose a task artifact update (the worker's result): one artifact whose
/// parts carry the result text.
#[must_use]
pub fn artifact_update(
    mesh: &MeshId,
    task_id: &TaskId,
    text: &str,
    seq: u64,
) -> TaskArtifactUpdate {
    artifact_update_parts(mesh, task_id, vec![Part::text(text)], seq)
}

/// As [`artifact_update`] but carrying arbitrary parts — used when the result
/// attaches a file (a `Part` with a blob `url`) alongside or instead of text.
#[must_use]
pub fn artifact_update_parts(
    mesh: &MeshId,
    task_id: &TaskId,
    parts: impl IntoIterator<Item = Part>,
    seq: u64,
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
        metadata: Some(serde_json::json!({ META_SEQ: seq })),
    }
}

/// The `mesh:seq` an artifact leg carries, when the peer sends one.
#[must_use]
pub fn artifact_seq(update: &TaskArtifactUpdate) -> Option<u64> {
    update.metadata.as_ref()?.get(META_SEQ)?.as_u64()
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

/// Seconds since the worker agent last checked in, as its beat states them.
#[must_use]
pub fn beat_checkin_age(update: &TaskStatusUpdate) -> Option<u64> {
    update.metadata.as_ref()?.get(META_CHECKIN_AGE)?.as_u64()
}

/// The progress a beat or a check-in carries, if any. A value outside 0 to 100 reads as none.
#[must_use]
pub fn leg_progress(update: &TaskStatusUpdate) -> Option<Progress> {
    let percent = update.metadata.as_ref()?.get(META_PROGRESS)?.as_u64()?;
    u8::try_from(percent)
        .ok()
        .and_then(|percent| Progress::try_from(percent).ok())
}

/// The initiator's one-line task label (`mesh:label`), when the brief carries one.
///
/// Peer-controlled text bound for a todo subject, so control characters
/// (a `\n` would forge a second row) are stripped and the length capped.
#[must_use]
pub fn task_label(message: &Message) -> Option<String> {
    clean_label(message.metadata.as_ref()?.get(META_LABEL)?.as_str()?)
}

/// `raw` as a task label: control characters stripped, length capped, `None`
/// when nothing is left.
#[must_use]
pub(crate) fn clean_label(raw: &str) -> Option<String> {
    clean_text(raw, super::tuning::TASK_LABEL_MAX_CHARS)
}

/// `raw` without its control characters, trimmed and then capped at `max` characters, and `None`
/// when nothing is left. The trim comes first, so a run of spaces in front cannot use up the cap
/// and leave nothing.
#[must_use]
pub(crate) fn clean_text(raw: &str, max: usize) -> Option<String> {
    let visible: String = raw.chars().filter(|ch| !ch.is_control()).collect();
    let capped: String = visible.trim().chars().take(max).collect();
    let trimmed = capped.trim_end();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// The `mesh:waiting_user` metadata of a `working` status leg that waits on its user, and none
/// for one that does not. The counterpart of [`status_waiting_user`].
#[must_use]
#[cfg(test)]
pub(crate) fn waiting_metadata(
    // `true` while the worker waits on its own user.
    waiting_user: bool,
) -> Option<serde_json::Value> {
    waiting_user.then(|| serde_json::json!({ META_WAITING_USER: true }))
}

/// The metadata of a status leg.
///
/// It holds the `mesh:seq` of the leg, `mesh:waiting_user` while the worker waits on its user, and
/// the progress of a check-in as a percentage. The counterpart of [`status_seq`],
/// [`status_waiting_user`] and [`leg_progress`].
#[must_use]
pub fn status_metadata(
    // `true` while the worker waits on its own user.
    waiting_user: bool,
    progress: Option<Progress>,
    seq: u64,
) -> serde_json::Value {
    let mut meta = serde_json::json!({ META_SEQ: seq });
    if waiting_user {
        meta[META_WAITING_USER] = true.into();
    }
    if let Some(progress) = progress {
        meta[META_PROGRESS] = progress.percent().into();
    }
    meta
}

/// The `mesh:seq` a status leg carries, when the peer sends one.
#[must_use]
pub fn status_seq(update: &TaskStatusUpdate) -> Option<u64> {
    update.metadata.as_ref()?.get(META_SEQ)?.as_u64()
}

/// Whether the status leg says the worker waits on its own user (`mesh:waiting_user`).
#[must_use]
pub fn status_waiting_user(update: &TaskStatusUpdate) -> bool {
    let flag = update
        .metadata
        .as_ref()
        .and_then(|meta| meta.get(META_WAITING_USER));
    flag.and_then(serde_json::Value::as_bool).unwrap_or(false)
}

/// The note of a status leg, the "what changed" line of a check-in.
///
/// Peer-controlled text that `GetTask` and `task_progress` echo, so it gets the
/// control-character strip of [`task_label`] and its own length cap.
#[must_use]
pub fn status_text(update: &TaskStatusUpdate) -> Option<String> {
    let message = update.status.message.as_ref()?;
    clean_text(
        &parts_text(&message.parts),
        super::tuning::TASK_STATUS_TEXT_MAX_CHARS,
    )
}

/// Whether `frame` is a `working` leg that breaks the check-in rule.
///
/// Such a leg is not a beat and lacks a text or a valid `mesh:progress`. The sender refuses to
/// emit one, and the receiver refuses to take one in.
#[must_use]
pub fn is_incomplete_checkin(frame: &Frame) -> bool {
    let Ok(payload) = status_payload(frame) else {
        return false;
    };
    payload.status.state == TaskState::Working
        && !is_beat(&payload)
        && (status_text(&payload).is_none() || leg_progress(&payload).is_none())
}

/// A check-in's progress: a whole percentage, 0 to 100.
///
/// Built only by a checked parse, so every value is in range. It goes on the
/// wire as the integer `mesh:progress`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Progress(u8);

impl Progress {
    /// The percentage.
    #[must_use]
    pub fn percent(self) -> u8 {
        self.0
    }

    /// The quarter of the work this progress is in, rounded down: 0 for 0-24,
    /// 1 for 25-49, 2 for 50-74, 3 for 75-99 and 4 for 100. A sender shows one
    /// glyph per quarter, so it wakes only when the quarter moves.
    #[must_use]
    pub fn quarter(self) -> u8 {
        if self.0 >= 100 { 4 } else { self.0 / 25 }
    }
}

impl TryFrom<u8> for Progress {
    type Error = String;

    fn try_from(percent: u8) -> Result<Self, Self::Error> {
        if percent <= 100 {
            Ok(Self(percent))
        } else {
            Err(format!("progress {percent} is not between 0 and 100"))
        }
    }
}

impl From<Progress> for u8 {
    fn from(progress: Progress) -> Self {
        progress.0
    }
}

impl std::str::FromStr for Progress {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let percent: u8 = text
            .parse()
            .map_err(|_| format!("progress `{text}` is not a whole number from 0 to 100"))?;
        Self::try_from(percent)
    }
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
    to: Option<habilis_network::protocol::AuthorKey>,
) -> Frame {
    let status = status_update(mesh, StatusUpdateParams::new(&TaskId::random(), state));
    Frame::new_app(
        mesh,
        &habilis_network::protocol::Nickname::from("author"),
        habilis_network::protocol::AppFrameParams::new(
            wire::app_tag(wire::STATUS),
            to,
            None,
            payload_body(&status).expect(
                "a status update with a fresh random id serializes to a body under the size cap",
            ),
        ),
    )
}

#[cfg(test)]
mod tests {
    use habilis_network::protocol::{MeshId, MessageId, MessageKind};

    use super::{
        BeatExtras, Frame, Progress, StatusUpdateParams, artifact_seq, artifact_update,
        beat_metadata_with_extras, broadcast_payload, compose_broadcast, compose_msg, display_text,
        leg_progress, msg_payload, payload_body, status_metadata, status_seq, status_update,
        status_waiting_user, waiting_metadata,
    };
    use crate::a2a::{META_PROGRESS, wire};

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
            MessageKind::app_to(
                wire::MSG,
                habilis_network::protocol::AuthorKey::from([0xad; 32]),
                None,
            ),
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
        assert_eq!(
            parsed, payload,
            "a receiver rebuilds exactly what the sender composed, or the two would disagree on content"
        );
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

    /// The writer of a leg (`status_metadata`, which the emit path puts on every status leg) and
    /// the reader (`status_waiting_user`) agree, and a leg that does not wait carries no flag.
    #[test]
    fn waiting_flag_round_trips_through_a_status_update() {
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
        assert!(
            status_waiting_user(&update(Some(status_metadata(true, None, 1)))),
            "the status payload is the one sent"
        );
        assert!(
            !status_waiting_user(&update(Some(status_metadata(false, None, 2)))),
            "a status that does not wait carries no flag"
        );
    }

    /// A status leg carries its seq next to the flag, and the reader still finds the flag. A leg
    /// from a peer without the seq reads as none.
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
        let leg = update(status_metadata(true, None, 7));
        assert_eq!(status_seq(&leg), Some(7), "the status seq is the one given");
        assert!(
            status_waiting_user(&leg),
            "the waiting flag is the one given"
        );
        assert_eq!(
            status_seq(&update(
                waiting_metadata(true).expect("the test metadata is valid")
            )),
            None,
            "the status payload carries its fields"
        );
    }

    /// A progress is a whole percentage from 0 to 100; anything else is a parse
    /// error, and an out-of-range value never reaches the wire.
    #[test]
    fn a_progress_is_a_whole_percentage() {
        for text in ["0", "55", "100"] {
            let parsed: Result<Progress, String> = text.parse();
            assert!(parsed.is_ok(), "{text} is inside the 0 to 100 range");
        }
        for text in ["101", "-1", "5.5", "50%", "", " 5", "fast"] {
            let parsed: Result<Progress, String> = text.parse();
            assert!(
                parsed.is_err(),
                "{text:?} is outside what a progress accepts"
            );
        }
        assert_eq!(
            status_metadata(false, Progress::try_from(100).ok(), 1)[META_PROGRESS],
            100,
            "a progress goes on the wire as a percentage"
        );
        assert!(
            serde_json::from_str::<Progress>("101").is_err(),
            "a progress above 100 does not deserialize"
        );
    }

    /// A sender shows one glyph per quarter: 0-24, 25-49, 50-74, 75-99, then 100.
    #[test]
    fn a_progress_quarter_rounds_down() {
        for (percent, quarter) in [
            (0, 0),
            (24, 0),
            (25, 1),
            (49, 1),
            (50, 2),
            (74, 2),
            (75, 3),
            (99, 3),
            (100, 4),
        ] {
            let progress = Progress::try_from(percent).expect("the test passes a valid percentage");
            assert_eq!(
                progress.quarter(),
                quarter,
                "quarters round down from {percent}%"
            );
        }
    }

    /// Only a whole percentage in `mesh:progress` counts: a value out of range, another type or
    /// the retired `mesh:done`/`mesh:total` pair reads as no progress.
    #[test]
    fn a_leg_carries_its_progress_as_one_percentage() {
        let update = |metadata: serde_json::Value| {
            status_update(
                &mesh(),
                StatusUpdateParams::new(
                    &crate::a2a::TaskId::random(),
                    crate::a2a::TaskState::Working,
                )
                .with_metadata(metadata),
            )
        };
        assert_eq!(
            leg_progress(&update(serde_json::json!({ META_PROGRESS: 40 }))),
            Progress::try_from(40).ok(),
            "a whole percentage in range is the wire shape of progress"
        );
        for metadata in [
            serde_json::json!({ META_PROGRESS: 101 }),
            serde_json::json!({ META_PROGRESS: -1 }),
            serde_json::json!({ META_PROGRESS: "40" }),
            serde_json::json!({ "mesh:done": 40, "mesh:total": 100 }),
            serde_json::json!({}),
        ] {
            assert_eq!(
                leg_progress(&update(metadata.clone())),
                None,
                "{metadata} does not read as a percentage"
            );
        }
    }
    /// A beat of the worker repeats the seq of its last leg and takes none of its own; a beat of
    /// the sender has none.
    #[test]
    fn a_beat_carries_the_seq_of_the_last_leg_only_when_given() {
        let with = beat_metadata_with_extras(BeatExtras {
            seq: Some(4),
            ..BeatExtras::default()
        });
        assert_eq!(
            with["mesh:seq"], 4,
            "the worker's beat repeats its last seq"
        );
        let without = beat_metadata_with_extras(BeatExtras::default());
        assert!(
            without.get("mesh:seq").is_none(),
            "the sender's beat has no seq"
        );
    }

    /// An artifact leg carries the seq it was built with.
    #[test]
    fn an_artifact_leg_carries_its_seq() {
        let task_id = crate::a2a::TaskId::random();
        let leg = artifact_update(&mesh(), &task_id, "the result", 6);
        assert_eq!(
            artifact_seq(&leg),
            Some(6),
            "the artifact reads back its seq"
        );
    }

    /// A note is trimmed before it is capped, so a run of spaces in front cannot use up the cap
    /// and leave nothing; control characters go, and a note of nothing else is none.
    #[test]
    fn a_note_is_trimmed_before_it_is_capped() {
        let padded = format!("{}text", " ".repeat(10));
        assert_eq!(
            super::clean_text(&padded, 4).as_deref(),
            Some("text"),
            "the cap counts the text, not the spaces in front of it"
        );
        assert_eq!(
            super::clean_text("ab\u{7}cd", 3).as_deref(),
            Some("abc"),
            "a control character is not a character of the note"
        );
        assert_eq!(
            super::clean_text("\u{7} \u{1b}", 9),
            None,
            "nothing is left"
        );
    }
}
