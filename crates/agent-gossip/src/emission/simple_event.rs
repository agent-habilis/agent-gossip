use habilis_network::protocol::messages::Authored;
use habilis_network::protocol::{AuthorKey, Channel, MessageKind, Nickname, PresenceSubtype};
use serde::Serialize;

use super::{CheckinNotice, GapNotice, OutputEvent, OverdueNotice, PingPeer, TaskGoneReason};
use crate::a2a::{status_update_params, wire};

/// The sigil that opens every human-readable `display` line. An
/// application-layer output convention, not an identifier prefix — mesh ids and
/// tickets are bare `Base58Check`. Carries the `\u{FE0F}` selector so terminals
/// render the colorful emoji rather than the monochrome text form.
const DISPLAY_GLYPH: &str = "💬\u{FE0F}";

/// One-shot events (everything except the `"event":"message"` family).
/// `#[serde(tag = "event")]` inlines the discriminator as the first field.
#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(super) enum SimpleEvent<'source> {
    Ready {
        version: &'source str,
        gossip: &'source str,
        name: &'source str,
        nickname: &'source str,
        /// Skill-drift warning for a stale install; omitted from the wire when
        /// the install is current, so the common case stays unchanged.
        #[serde(skip_serializing_if = "Option::is_none")]
        drift: Option<&'source str>,
        /// The bound `--a2a-serve` port; omitted when the binding is off.
        #[serde(skip_serializing_if = "Option::is_none")]
        a2a_port: Option<u16>,
    },
    PeerTimeout {
        nickname: &'source str,
        /// The author key of the peer, hex: two peers can share a nickname.
        pubkey: &'source str,
        last_seen_secs_ago: u64,
        /// Pre-formatted, markdown-safe line the `/mesh` skill echoes
        /// verbatim. See [`peer_timeout_display`].
        display: String,
    },
    PeerReturn {
        nickname: &'source str,
        /// The author key of the peer, hex.
        pubkey: &'source str,
        /// Pre-formatted, markdown-safe line (see [`peer_return_display`]).
        display: String,
    },
    TaskTimeout {
        task_id: &'source str,
        reason: TaskGoneReason,
    },
    TaskOverdue {
        #[serde(flatten)]
        notice: &'source OverdueNotice,
        /// Pre-formatted line (see [`task_overdue_event`]).
        display: String,
    },
    CheckinDue {
        #[serde(flatten)]
        notice: &'source CheckinNotice,
        display: String,
    },
    TaskGap {
        #[serde(flatten)]
        notice: &'source GapNotice,
        display: String,
    },
    Receipt {
        from: &'source str,
        #[serde(rename = "ref")]
        ref_id: &'source str,
        state: &'source str,
        ts: u64,
    },
    Info {
        message: &'source str,
    },
    Error {
        message: &'source str,
    },
    NicknameHeld {
        nickname: &'source str,
        /// Pre-formatted line (see [`nickname_held_display`]).
        display: String,
    },
    EventsLost {
        lost: u64,
        /// Pre-formatted line (see [`events_lost_display`]).
        display: String,
    },
    PingReport {
        peers: &'source [PingPeer],
        responded: usize,
        known: usize,
        /// Pre-formatted, markdown-safe RTT table (see [`ping_report_display`]).
        display: String,
    },
}

/// Common prefix for every `{"event":"message",...}` line. Field
/// order is part of the wire format.
#[derive(Serialize)]
struct MessageHeader<'source> {
    pub event: &'static str,
    pub id: &'source str,
    #[serde(rename = "type")]
    pub ty: &'static str,
    pub gossip: &'source str,
    pub author: &'source str,
    /// Author's full Ed25519 public key (hex) — the cryptographic identity
    /// behind the display `author`. Agents should key
    /// trust/disambiguation on this, not the nickname.
    pub pubkey: String,
    pub ts: i64,
}

#[derive(Serialize)]
struct MsgLine<'source> {
    #[serde(flatten)]
    pub header: MessageHeader<'source>,
    /// The plain-text projection of the A2A payload (text parts joined) —
    /// the convenience field agents read without unpacking `message`.
    pub body: String,
    pub to: Option<&'source str>,
    /// The addressee's key, always present with `to`: the machine-readable identity of `to`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_pubkey: Option<&'source str>,
    /// The embedded A2A `Message` object the frame carried — the full
    /// payload (parts, contextId, extensions, metadata) for A2A-aware
    /// consumers. `None` only for an unparseable payload, which the receive
    /// path already drops; it survives here so a display path can never
    /// panic on crafted input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<crate::a2a::Message>,
    /// Pre-formatted, markdown-safe line the `/mesh` skill echoes
    /// verbatim — the single source of truth for what the user sees.
    /// See [`msg_display`].
    pub display: String,
    #[serde(rename = "self")]
    pub is_self: bool,
}

#[derive(Serialize)]
struct PresenceLine<'source> {
    #[serde(flatten)]
    pub header: MessageHeader<'source>,
    pub subtype: PresenceSubtype,
    /// Pre-formatted, markdown-safe line (see [`presence_display`]).
    pub display: String,
}

/// A `{"event":"task",...}` line for a **content** task leg. A distinct
/// top-level event (not the `message` family) so skills branch on `event`;
/// field order is part of the wire format. `kind` is the native A2A construct
/// (`"message"` / `"status-update"` / `"artifact-update"`), `state` the task's
/// A2A state; `payload` is the construct whole for A2A-aware consumers, except
/// that a `message` leg drops the text parts `body` already holds.
#[derive(Serialize)]
struct TaskLine<'source> {
    pub event: &'static str,
    pub id: &'source str,
    pub gossip: &'source str,
    pub author: &'source str,
    pub pubkey: String,
    pub ts: i64,
    pub to: &'source str,
    /// The addressee's key. Absent on a leg that arrived over the request/response binding,
    /// which carries the addressee's name only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_pubkey: Option<&'source str>,
    pub task_id: String,
    pub kind: &'static str,
    /// The friendly kebab state (`working`/`input-required`/…) — our agent API,
    /// not the A2A wire's `ProtoJSON` `TASK_STATE_*` (which rides `payload`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<&'static str>,
    /// The initiator's one-line name for the task, lifted out of the brief's
    /// `mesh:label` metadata so both parties label it the same way. Rides the
    /// opening brief only, and is absent from a peer that does not set it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<&'source str>,
    /// `true` while the worker waits on its own user, from the leg's `mesh:waiting_user`.
    /// Absent on every other leg, so the leg that clears it carries no key.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub waiting_user: bool,
    /// How far the work is, 0 to 100, from a check-in. Absent on a leg with none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// Pre-formatted, markdown-safe line (see [`task_display`]).
    pub display: String,
    #[serde(rename = "self")]
    pub is_self: bool,
}

/// A `{"event":"task_progress",...}` line for a liveness beat — the
/// ball-owner's keepalive heartbeat. `progress` is `None` when the beat reports indeterminate
/// progress (no percentage in its metadata).
#[derive(Serialize)]
struct TaskProgressLine<'source> {
    pub event: &'static str,
    pub id: &'source str,
    pub gossip: &'source str,
    pub author: &'source str,
    pub ts: i64,
    pub to: &'source str,
    pub to_pubkey: &'source str,
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    pub display: String,
    #[serde(rename = "self")]
    pub is_self: bool,
}

/// A `{"event":"state",...}` line for a shared-state change. Its own top-level
/// event (not the `message` family) so skills branch on `event`. Carries, on the
/// `state` channel, the freshly-derived document; field order is part of the wire format.
#[derive(Serialize)]
struct StateLine<'source> {
    event: &'static str,
    id: &'source str,
    #[serde(rename = "type")]
    ty: &'static str,
    gossip: &'source str,
    author: &'source str,
    pubkey: String,
    ts: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    document: Option<&'source serde_json::Value>,
    display: String,
    #[serde(rename = "self")]
    is_self: bool,
}

fn message_header<'a>(message: &'a Authored, ty: &'static str) -> MessageHeader<'a> {
    let pubkey = message.key().to_string();
    MessageHeader {
        event: "message",
        id: message.id.as_str(),
        ty,
        gossip: message.mesh.as_str(),
        author: message.author.as_str(),
        pubkey,
        ts: message.timestamp.as_secs(),
    }
}

/// The pre-formatted, markdown-safe `display` line for a chat event —
/// the single source of truth the `/mesh` skill echoes verbatim, so the
/// model never composes or re-types a body. Nicks are wrapped in literal
/// backticks: the skill renders into markdown, where a bare `<nick>` is
/// stripped as an HTML tag, and the code span prevents that. The body is
/// embedded **raw** (never trimmed, escaped, or re-spaced).
///
/// `to` is the msg addressee, and the `→` it renders is the only
/// thing distinguishing a private line from a gossip-wide one on screen.
fn msg_display(author: &str, body: &str, to: Option<&str>) -> String {
    match to {
        Some(target) => format!("{DISPLAY_GLYPH} `<{author}>` → `<{target}>`: {body}"),
        None => format!("{DISPLAY_GLYPH} `<{author}>`: {body}"),
    }
}

/// The `to` text of a directed line: the addressee's name while it is live, else its key hex.
fn to_label(key: &AuthorKey, name: Option<&Nickname>) -> String {
    name.map_or_else(|| key.to_string(), |name| name.as_str().to_owned())
}

/// The value cluster for [`task_display`]: the author/target nicks, the
/// native A2A `kind`/`state`, and the text body.
#[derive(Clone, Copy)]
struct TaskDisplayParams<'caller> {
    author: &'caller str,
    to: &'caller str,
    kind: &'caller str,
    state: Option<crate::a2a::TaskState>,
    body: &'caller str,
}

/// `display` line for a `task` event:
/// `` 💬️ task offer `<author>` → `<to>`: body ``. See
/// [`msg_display`] for the backtick rationale. The skill may render a
/// richer interaction (the tasks widget, collapsed status lines) instead
/// of echoing this verbatim; it is the canonical line for raw
/// `--output json` consumers.
fn task_display(params: TaskDisplayParams<'_>) -> String {
    let TaskDisplayParams {
        author,
        to,
        kind,
        state,
        body,
    } = params;
    let label = state.map_or_else(|| kind.to_owned(), |state| format!("{kind} {state}"));
    format!("{DISPLAY_GLYPH} task {label} `<{author}>` → `<{to}>`: {body}")
}

/// `display` line for a `task_progress` event:
/// `` 💬️ task progress `<author>` → `<to>`: 35% `` (or
/// `working` when indeterminate).
fn task_progress_display(author: &str, to: &str, progress: Option<u8>) -> String {
    match progress {
        Some(percent) => {
            format!("{DISPLAY_GLYPH} task progress `<{author}>` → `<{to}>`: {percent}%")
        }
        None => format!("{DISPLAY_GLYPH} task progress `<{author}>` → `<{to}>`: working"),
    }
}

/// `display` line for a presence event: `` 💬️ `<author>` has joined `` /
/// `… has left`. See [`msg_display`] for the backtick rationale.
fn presence_display(author: &str, subtype: PresenceSubtype) -> String {
    format!("{DISPLAY_GLYPH} `<{author}>` has {subtype}")
}

/// `display` line for a `peer_timeout` event.
fn peer_timeout_display(nickname: &str) -> String {
    format!("{DISPLAY_GLYPH} `<{nickname}>` went quiet")
}

/// The `task_overdue` event. Its `display` states the silence as a fact; each
/// bracketed part appears only when it is set.
pub(super) fn task_overdue_event(notice: &OverdueNotice) -> SimpleEvent<'_> {
    let OverdueNotice {
        task_id,
        peer,
        label,
        age_secs,
        waiting_user,
        not_picked_up,
    } = notice;
    let short_id: String = task_id.as_str().chars().take(8).collect();
    let name = label.clone().unwrap_or(short_id);
    let silence = if *age_secs < 60 {
        format!("{age_secs} s")
    } else {
        format!("{} min", age_secs / 60)
    };
    let mut parts = vec![format!(
        "{DISPLAY_GLYPH} task overdue · `<{peer}>` · {name} · no check-in for {silence}"
    )];
    if *waiting_user {
        parts.push(" · waiting on user".to_owned());
    }
    if *not_picked_up {
        parts.push(" · not picked up".to_owned());
    }
    SimpleEvent::TaskOverdue {
        notice,
        display: parts.concat(),
    }
}

/// The `checkin_due` event: the worker's reminder. It is never shown, so its
/// `display` only keeps the line readable in a raw poll.
pub(super) fn checkin_due_event(notice: &CheckinNotice) -> SimpleEvent<'_> {
    SimpleEvent::CheckinDue {
        notice,
        display: format!("{DISPLAY_GLYPH} check-in due · `<{}>`", notice.peer),
    }
}

/// The `task_gap` event: a question of the worker was lost on the way. It is never shown, so its
/// `display` only keeps the line readable in a raw poll.
pub(super) fn task_gap_event(notice: &GapNotice) -> SimpleEvent<'_> {
    SimpleEvent::TaskGap {
        notice,
        display: format!("{DISPLAY_GLYPH} task gap · `<{}>`", notice.peer),
    }
}

/// `display` line for a `peer_return` event.
fn peer_return_display(nickname: &str) -> String {
    format!("{DISPLAY_GLYPH} `<{nickname}>` came back")
}

fn nickname_held_display(nickname: &str) -> String {
    format!(
        "`{nickname}` is held by another member: this node is not admitted under it; join again without --nickname, or choose another one"
    )
}

/// `display` line for an `events_lost` event.
fn events_lost_display(lost: u64) -> String {
    let events = if lost == 1 {
        "event was"
    } else {
        "events were"
    };
    format!("{DISPLAY_GLYPH} {lost} {events} lost before the poll history")
}

/// `display` block for a `ping_report` event: a markdown RTT table (one
/// row per responding peer), or a single line when no peer answered.
fn ping_report_display(peers: &[PingPeer], known: usize) -> String {
    use std::fmt::Write as _;

    if peers.is_empty() {
        return format!("{DISPLAY_GLYPH} ping: no peers responded");
    }
    let mut out = format!("{DISPLAY_GLYPH} ping\n| peer | RTT |\n|---|---|\n");
    for peer in peers {
        let _ = writeln!(out, "| `<{}>` | {}ms |", peer.nickname, peer.rtt_ms);
    }
    let _ = write!(out, "{}/{known} online", peers.len());
    out
}

/// Write a line to stdout and flush immediately.
/// Required for Monitor compatibility — piped stdout is fully buffered,
/// so without explicit flush, events stall in an 8KB buffer.
pub(super) fn emit(line: &str) {
    use std::io::Write;

    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{line}");
    let _ = lock.flush();
}

/// Whether an event's `display` line belongs in the agent's transcript — the
/// daemon-owned print decision, stamped on every JSON line as `is_visible` so
/// no skill re-derives a skip-list. Everything else in a poll batch is
/// context, not output: state/meta document echoes (documents are on-demand
/// via `state get`/`meta get`), task legs (interactions the task flow
/// drives), presence `alive` beats, and the operational stream-only events.
pub(crate) fn is_visible(event: &OutputEvent) -> bool {
    match event {
        OutputEvent::Message { .. }
        | OutputEvent::PingReport { .. }
        | OutputEvent::PeerTimeout { .. }
        | OutputEvent::PeerReturn { .. }
        | OutputEvent::EventsLost { .. }
        | OutputEvent::NicknameHeld { .. }
        | OutputEvent::TaskOverdue { .. } => true,
        OutputEvent::Presence { message, .. } => matches!(
            &message.kind,
            MessageKind::Presence {
                subtype: PresenceSubtype::Joined | PresenceSubtype::Left
            }
        ),
        OutputEvent::Task { .. }
        | OutputEvent::TaskMessage { .. }
        | OutputEvent::TaskTimeout { .. }
        | OutputEvent::CheckinDue { .. }
        | OutputEvent::TaskGap { .. }
        | OutputEvent::Receipt { .. }
        | OutputEvent::StateChanged { .. }
        | OutputEvent::Info { .. }
        | OutputEvent::Error { .. }
        | OutputEvent::Ready { .. } => false,
    }
}

/// Append `"is_visible":<b>` as the last field of an already-rendered event
/// object. Spliced (like `seq` in [`surfaced_event_json`]) rather than added
/// to every line struct; re-parsing to a `Value` would reorder the pinned
/// fields.
fn stamp_visibility(line: String, is_visible: bool) -> String {
    match line.strip_suffix('}') {
        Some(body) => format!("{body},\"is_visible\":{is_visible}}}"),
        None => line,
    }
}

/// Format a presence message as JSON.
///
/// Serializes the struct directly because the documented wire format
/// pins the field order (`event`, `id`, `type`, `gossip`, `author`,
/// `pubkey`, `ts`, …) and `Value::to_string` would sort keys alphabetically.
pub(super) fn format_presence_json(
    message: &Authored,
    subtype: PresenceSubtype,
    peer: &AuthorKey,
) -> String {
    // Presence carries no body — peer model/harness/host lives in the `meta` channel.
    let line = serde_json::to_string(&PresenceLine {
        header: MessageHeader {
            pubkey: peer.to_string(),
            ..message_header(message, "presence")
        },
        subtype,
        display: presence_display(message.author.as_str(), subtype),
    })
    .expect("presence event serialization should never fail");
    stamp_visibility(
        line,
        matches!(subtype, PresenceSubtype::Joined | PresenceSubtype::Left),
    )
}

/// Format a chat frame as a JSON string — either tag. Presence uses
/// `format_presence_json`; `PeerInfo` is never printed.
///
/// The two tags differ in exactly two rendered fields, and both are how a
/// consumer tells a private line from a public one: `type` (`"broadcast"` vs
/// `"msg"`) and `to` (`null` vs the addressee, which also turns on the `→` in
/// `display`).
pub(super) fn format_msg_json(
    message: &Authored,
    is_self: bool,
    addressee_name: Option<&Nickname>,
) -> String {
    let MessageKind::App { tag, to, .. } = &message.kind else {
        unreachable!("format_msg_json only handles chat frames")
    };
    let to = match tag.as_str() {
        wire::MSG => to.as_ref(),
        wire::BROADCAST => None,
        _ => unreachable!("format_msg_json only handles chat frames"),
    };
    // Inbound frames were validated at the receive boundary and our own echoes
    // are built by `send_broadcast`/`send_msg`, so the parse succeeds in
    // practice; the fallback keeps a display path from ever panicking on a
    // crafted body.
    let payload = serde_json::from_str::<crate::a2a::Message>(message.body.as_str()).ok();
    let body = payload.as_ref().map_or_else(
        || message.body.as_str().to_owned(),
        status_update_params::display_text,
    );
    let to_text = to.map(|key| to_label(key, addressee_name));
    let to_pubkey = to.map(ToString::to_string);
    stamp_visibility(
        serde_json::to_string(&MsgLine {
            header: message_header(
                message,
                if to_text.is_some() {
                    "msg"
                } else {
                    "broadcast"
                },
            ),
            display: msg_display(message.author.as_str(), &body, to_text.as_deref()),
            body,
            to: to_text.as_deref(),
            to_pubkey: to_pubkey.as_deref(),
            message: payload,
            is_self,
        })
        .expect("message event serialization should never fail"),
        true,
    )
}

/// Format a worker-pushed task frame (an `a2a_status` or `a2a_artifact`) as
/// its JSON line. A beat renders as a `task_progress` event; every other leg
/// as a `task` event carrying the native A2A `kind` + `state`, the text
/// projection as `body`, and the whole A2A payload.
pub(super) fn format_task_json(
    message: &Authored,
    is_self: bool,
    addressee_name: Option<&Nickname>,
) -> String {
    let MessageKind::App { tag, to, .. } = &message.kind else {
        unreachable!("format_task_json only handles status/artifact frames")
    };
    if !matches!(tag.as_str(), wire::STATUS | wire::ARTIFACT) {
        unreachable!("format_task_json only handles status/artifact frames");
    }
    let Some(to) = to else {
        unreachable!("a status/artifact frame is always directed")
    };
    let task_id = status_update_params::frame_task_id(message)
        .expect("a task frame carries its task id")
        .as_str()
        .to_owned();
    // A liveness beat (a status marked `mesh:beat`) → task_progress widget.
    if let Ok(payload) = status_update_params::status_payload(message)
        && status_update_params::is_beat(&payload)
    {
        let progress = status_update_params::leg_progress(&payload)
            .map(status_update_params::Progress::percent);
        let author = message.author.as_str();
        let to_text = to_label(to, addressee_name);
        let to_pubkey = to.to_string();
        let display = task_progress_display(author, &to_text, progress);
        return stamp_visibility(
            serde_json::to_string(&TaskProgressLine {
                event: "task_progress",
                id: message.id.as_str(),
                gossip: message.mesh.as_str(),
                author,
                ts: message.timestamp.as_secs(),
                to: &to_text,
                to_pubkey: &to_pubkey,
                task_id,
                progress,
                display,
                is_self,
            })
            .expect("task_progress event serialization should never fail"),
            false,
        );
    }
    let kind = status_update_params::task_event_kind(message).unwrap_or("status-update");
    let state = status_update_params::frame_task_state(message);
    let body = status_update_params::task_text(message);
    let author = message.author.as_str();
    let to_text = to_label(to, addressee_name);
    let to_pubkey = to.to_string();
    let pubkey = message.key().to_string();
    // Only a `working` leg carries the tag and the progress, as in the record.
    let checkin = status_update_params::status_payload(message)
        .ok()
        .filter(|payload| payload.status.state == crate::a2a::TaskState::Working);
    let waiting_user = checkin
        .as_ref()
        .is_some_and(status_update_params::status_waiting_user);
    let progress: Option<u8> = checkin
        .as_ref()
        .and_then(status_update_params::leg_progress)
        .map(status_update_params::Progress::percent);
    let display = task_display(TaskDisplayParams {
        author,
        to: &to_text,
        kind,
        state,
        body: &body,
    });
    let payload: Option<serde_json::Value> = serde_json::from_str(message.body.as_str()).ok();
    stamp_visibility(
        serde_json::to_string(&TaskLine {
            event: "task",
            id: message.id.as_str(),
            gossip: message.mesh.as_str(),
            author,
            pubkey,
            ts: message.timestamp.as_secs(),
            to: &to_text,
            to_pubkey: Some(&to_pubkey),
            task_id,
            kind,
            state: state.map(crate::a2a::TaskState::friendly_name),
            // The label rides the opening brief, which arrives over the RPC
            // binding (`OutputEvent::TaskMessage`), never as a frame.
            label: None,
            waiting_user,
            progress,
            display,
            body,
            payload,
            is_self,
        })
        .expect("task event serialization should never fail"),
        false,
    )
}

/// The leg's `Message` without its text parts: the text is already in `body`
/// and `display`, and a third copy would triple a long brief in every poll.
fn message_without_text(message: &crate::a2a::Message) -> Option<serde_json::Value> {
    let mut value = serde_json::to_value(message).ok()?;
    if let Some(serde_json::Value::Array(parts)) = value.get_mut("parts") {
        parts.retain(|part| part.get("text").is_none());
    }
    Some(value)
}

/// `display` line for a `state` event: `` 💬️ `<author>` changed shared state ``,
/// or `💬️ you changed shared state` for your own write. A peer's nick is
/// backtick-wrapped like every other event so the skill's markdown renderer keeps
/// the `<nick>`; "you" is plain text. The slice of an author replaces its old
/// slice, so the event holds no delta to name.
fn state_display(author: &str, is_self: bool) -> String {
    if is_self {
        format!("{DISPLAY_GLYPH} you changed shared state")
    } else {
        format!("{DISPLAY_GLYPH} `<{author}>` changed shared state")
    }
}

/// Whether a poll reader gets the document of a change on `channel`: in the JSON line, and in the
/// ring copy of the event that the JSON line is made from. The Meta document is every peer's agent
/// card, ~4k tokens with four peers, repeated on each change; readers take it on demand with
/// `meta get`. The live copy of a change (the capture stream) keeps the document for in-process readers.
/// This is the one place that holds the rule.
pub(super) fn poll_carries_document(channel: Channel) -> bool {
    channel != Channel::Meta
}

/// Render a `StateChanged` event as its `{"event":"state",...}` JSON line: the
/// header, the freshly-derived `document`, the `display` line, and `self`.
pub(super) fn format_state_json(
    channel: Channel,
    event: &Authored,
    document: Option<&serde_json::Value>,
    is_self: bool,
) -> String {
    let label = channel.label();
    let author = event.author.as_str();
    let pubkey = event.key().to_string();
    stamp_visibility(
        serde_json::to_string(&StateLine {
            event: label,
            id: event.id.as_str(),
            ty: label,
            gossip: event.mesh.as_str(),
            author,
            pubkey,
            ts: event.timestamp.as_secs(),
            document: document.filter(|_| poll_carries_document(channel)),
            display: state_display(author, is_self),
            is_self,
        })
        .expect("state event serialization should never fail"),
        false,
    )
}

/// Render a `seq`-tagged surfaced event to the exact stream JSON line.
///
/// The daemon-local `seq` is flattened in as a leading field so a `poll` client can
/// advance its `--after` cursor. The body after `seq` is byte-identical to the
/// live `--output json` line for the same event (same [`event_json`]
/// renderer) — the parity guarantee `poll` rests on. `None` for events that
/// produce no JSON line.
#[must_use]
pub fn surfaced_event_json(seq: u64, event: &OutputEvent) -> Option<String> {
    let line = event_json(event)?;
    // `line` is a JSON object string starting with `{`. Splice `"seq":N,`
    // right after the opening brace so `seq` leads and the rest is unchanged.
    // (Re-parsing to a Value would reorder keys; the wire format pins order.)
    let rest = line.strip_prefix('{')?;
    let sep = if rest.starts_with('}') { "" } else { "," };
    Some(format!("{{\"seq\":{seq}{sep}{rest}"))
}

/// Render an [`OutputEvent`] to the exact JSON line the daemon writes in `--output json` mode.
///
/// The `Stream` sink prints the line that this function gives, so in-process tests assert the
/// byte-identical wire format the `/mesh` skill + MCP clients parse. `None` for events
/// that produce no JSON line in JSON mode.
#[must_use]
pub fn event_json(event: &OutputEvent) -> Option<String> {
    formatted_json(event)
        .or_else(|| serialized_json(event).map(|line| stamp_visibility(line, is_visible(event))))
}

/// The events whose line a formatter builds. `None` for the serialized ones, and for a formatted
/// event with no line: [`serialized_json`] gives `None` for that one too, so it stays silent.
fn formatted_json(event: &OutputEvent) -> Option<String> {
    let line = match event {
        OutputEvent::Message {
            message,
            is_self,
            to_name,
        } => format_msg_json(message, *is_self, to_name.as_ref()),
        OutputEvent::Task {
            message,
            is_self,
            to_name,
            ..
        } => format_task_json(message, *is_self, to_name.as_ref()),
        OutputEvent::TaskMessage {
            id,
            ts,
            mesh,
            author,
            author_key,
            peer,
            task_id,
            state,
            text,
            label,
            message,
            is_self,
        } => {
            let display = task_display(TaskDisplayParams {
                author,
                to: peer,
                kind: "message",
                state: *state,
                body: text,
            });
            let payload = message.as_deref().and_then(message_without_text);
            let pubkey = author_key.to_string();
            let ts = ts.as_secs();
            let line = TaskLine {
                event: "task",
                id,
                gossip: mesh,
                author,
                pubkey,
                to_pubkey: None,
                ts,
                to: peer,
                task_id: task_id.clone(),
                kind: "message",
                state: state.map(crate::a2a::TaskState::friendly_name),
                label: label.as_deref(),
                waiting_user: false,
                progress: None,
                display,
                body: text.clone(),
                payload,
                is_self: *is_self,
            };
            stamp_visibility(
                serde_json::to_string(&line).expect("task event serialization should never fail"),
                false,
            )
        }
        OutputEvent::Presence { message, peer } => {
            let MessageKind::Presence { subtype } = &message.kind else {
                return None;
            };
            format_presence_json(message, *subtype, peer)
        }
        OutputEvent::StateChanged {
            channel,
            event,
            document,
            is_self,
        } => format_state_json(*channel, event, document.as_deref(), *is_self),
        OutputEvent::Ready { .. }
        | OutputEvent::TaskTimeout { .. }
        | OutputEvent::TaskOverdue { .. }
        | OutputEvent::CheckinDue { .. }
        | OutputEvent::TaskGap { .. }
        | OutputEvent::Receipt { .. }
        | OutputEvent::PeerTimeout { .. }
        | OutputEvent::PeerReturn { .. }
        | OutputEvent::Info { .. }
        | OutputEvent::Error { .. }
        | OutputEvent::NicknameHeld { .. }
        | OutputEvent::EventsLost { .. }
        | OutputEvent::PingReport { .. } => return None,
    };
    Some(line)
}

/// The events serialized straight from [`SimpleEvent`]. `None` for the formatted ones.
fn serialized_json(event: &OutputEvent) -> Option<String> {
    match event {
        OutputEvent::Ready {
            mesh,
            name,
            nickname,
            drift,
            a2a_port,
        } => serde_json::to_string(&SimpleEvent::Ready {
            version: crate::version(),
            gossip: mesh.as_str(),
            name: name.as_str(),
            nickname: nickname.as_str(),
            drift: drift.as_deref(),
            a2a_port: *a2a_port,
        })
        .ok(),
        OutputEvent::TaskTimeout { task_id, reason } => {
            serde_json::to_string(&SimpleEvent::TaskTimeout {
                task_id: task_id.as_str(),
                reason: *reason,
            })
            .ok()
        }
        OutputEvent::TaskOverdue { notice } => {
            serde_json::to_string(&task_overdue_event(notice)).ok()
        }
        OutputEvent::CheckinDue { notice } => {
            serde_json::to_string(&checkin_due_event(notice)).ok()
        }
        OutputEvent::TaskGap { notice } => serde_json::to_string(&task_gap_event(notice)).ok(),
        OutputEvent::Receipt {
            from,
            ref_id,
            state,
            ts,
        } => serde_json::to_string(&SimpleEvent::Receipt {
            from: from.as_str(),
            ref_id,
            state,
            ts: *ts,
        })
        .ok(),
        OutputEvent::PeerTimeout {
            nickname,
            key,
            last_seen_secs_ago,
        } => serde_json::to_string(&SimpleEvent::PeerTimeout {
            nickname: nickname.as_str(),
            pubkey: key.to_string().as_str(),
            last_seen_secs_ago: *last_seen_secs_ago,
            display: peer_timeout_display(nickname.as_str()),
        })
        .ok(),
        OutputEvent::PeerReturn { nickname, key } => {
            serde_json::to_string(&SimpleEvent::PeerReturn {
                nickname: nickname.as_str(),
                pubkey: key.to_string().as_str(),
                display: peer_return_display(nickname.as_str()),
            })
            .ok()
        }
        OutputEvent::Info { message } => serde_json::to_string(&SimpleEvent::Info { message }).ok(),
        OutputEvent::Error { message } => {
            serde_json::to_string(&SimpleEvent::Error { message }).ok()
        }
        OutputEvent::NicknameHeld { nickname } => {
            serde_json::to_string(&SimpleEvent::NicknameHeld {
                nickname,
                display: nickname_held_display(nickname),
            })
            .ok()
        }
        OutputEvent::EventsLost { lost } => serde_json::to_string(&SimpleEvent::EventsLost {
            lost: *lost,
            display: events_lost_display(*lost),
        })
        .ok(),
        OutputEvent::PingReport { peers, known } => {
            serde_json::to_string(&SimpleEvent::PingReport {
                responded: peers.len(),
                display: ping_report_display(peers, *known),
                peers,
                known: *known,
            })
            .ok()
        }
        OutputEvent::Message { .. }
        | OutputEvent::Task { .. }
        | OutputEvent::TaskMessage { .. }
        | OutputEvent::Presence { .. }
        | OutputEvent::StateChanged { .. } => None,
    }
}
