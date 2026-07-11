//! The JSON wire layer: the serde shapes for every line the daemon
//! emits on stdout (consumed by the `/mesh` skill and any `--output
//! json` client) plus the serializers that render them. Field
//! order/naming is part of the wire format — documented in AGENTS.md,
//! pinned by the insta snapshots in `tests`. The `Output` sink in the
//! parent module renders through these so the captured-event and
//! stdout forms stay byte-identical.

use std::fmt::Write as _;
use std::io::Write;

use serde::Serialize;

use agent_habilis_mesh::util::consts::MESH_GLYPH;

use super::{OutputEvent, TaskMessageLeg};
use agent_habilis_mesh::protocol::{Message, MessageKind, Nickname, PresenceSubtype};

/// One-shot events (everything except the `"event":"message"` family).
/// `#[serde(tag = "event")]` inlines the discriminator as the first field.
#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(super) enum SimpleEvent<'a> {
    Ready {
        version: &'a str,
        square: &'a str,
        name: &'a str,
        nickname: &'a str,
        /// Skill-drift warning for a stale install; omitted from the wire when
        /// the install is current, so the common case stays unchanged.
        #[serde(skip_serializing_if = "Option::is_none")]
        drift: Option<&'a str>,
        /// The bound `--a2a-serve` port; omitted when the binding is off.
        #[serde(skip_serializing_if = "Option::is_none")]
        a2a_port: Option<u16>,
    },
    MsgPosted {
        id: &'a str,
    },
    PeerTimeout {
        nickname: &'a str,
        last_seen_secs_ago: u64,
        /// Pre-formatted, markdown-safe line the `/mesh` skill echoes
        /// verbatim. See [`peer_timeout_display`].
        display: String,
    },
    PeerReturn {
        nickname: &'a str,
        /// Pre-formatted, markdown-safe line (see [`peer_return_display`]).
        display: String,
    },
    Fork {
        nickname: &'a str,
        pubkey: &'a str,
        seq: u64,
    },
    TaskTimeout {
        task_id: &'a str,
    },
    Info {
        message: &'a str,
    },
    Error {
        message: &'a str,
    },
    PingReport {
        peers: Vec<PingPeer>,
        responded: usize,
        known: usize,
        /// Pre-formatted, markdown-safe RTT table (see [`ping_report_display`]).
        display: String,
    },
}

/// One peer's RTT in a `ping_report` event.
#[derive(Debug, Clone, Serialize)]
pub struct PingPeer {
    pub nickname: Nickname,
    pub rtt_ms: u64,
}

/// Common prefix for every `{"event":"message",...}` line. Field
/// order is part of the wire format (see AGENTS.md).
#[derive(Serialize)]
struct MessageHeader<'a> {
    pub event: &'static str,
    pub id: &'a str,
    #[serde(rename = "type")]
    pub ty: &'static str,
    pub square: &'a str,
    pub author: &'a str,
    /// Author's full Ed25519 public key (hex) — the cryptographic identity
    /// behind the display `author`. `Some` on every signed (real) message;
    /// `None` (omitted) only for unsigned test fixtures. Agents should key
    /// trust/disambiguation on this, not the nickname.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pubkey: Option<&'a str>,
    pub ts: i64,
}

#[derive(Serialize)]
struct MsgLine<'a> {
    #[serde(flatten)]
    pub header: MessageHeader<'a>,
    /// The plain-text projection of the A2A payload (text parts joined) —
    /// the convenience field agents read without unpacking `message`.
    pub body: String,
    pub to: Option<&'a str>,
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
struct PresenceLine<'a> {
    #[serde(flatten)]
    pub header: MessageHeader<'a>,
    pub subtype: PresenceSubtype,
    /// Pre-formatted, markdown-safe line (see [`presence_display`]).
    pub display: String,
}

/// A `{"event":"task",...}` line for a **content** task leg. A distinct
/// top-level event (not the `message` family) so skills branch on `event`;
/// field order is part of the wire format. `kind` is the native A2A construct
/// (`"message"` / `"status-update"` / `"artifact-update"`), `state` the task's
/// A2A state; `payload` is the construct whole for A2A-aware consumers.
#[derive(Serialize)]
struct TaskLine<'a> {
    pub event: &'static str,
    pub id: &'a str,
    pub square: &'a str,
    pub author: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pubkey: Option<&'a str>,
    pub ts: i64,
    pub to: &'a str,
    pub task_id: String,
    pub kind: &'static str,
    /// The friendly kebab state (`working`/`input-required`/…) — our agent API,
    /// not the A2A wire's `ProtoJSON` `TASK_STATE_*` (which rides `payload`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<&'static str>,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// Pre-formatted, markdown-safe line (see [`task_display`]).
    pub display: String,
    #[serde(rename = "self")]
    pub is_self: bool,
}

/// A `{"event":"task_progress",...}` line for a liveness beat — the
/// ball-owner's keepalive/percent heartbeat. `done`/`total` are `None` when
/// the beat reports indeterminate progress (no fraction in its metadata).
#[derive(Serialize)]
struct TaskProgressLine<'a> {
    pub event: &'static str,
    pub id: &'a str,
    pub square: &'a str,
    pub author: &'a str,
    pub ts: i64,
    pub to: &'a str,
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    pub display: String,
    #[serde(rename = "self")]
    pub is_self: bool,
}

/// A `{"event":"state",...}` line for a shared-state change. Its own top-level
/// event (not the `message` family) so skills branch on `event`. Carries the
/// merge delta and the freshly-derived document; field order is part of the
/// wire format.
#[derive(Serialize)]
struct StateLine<'a> {
    event: &'static str,
    id: &'a str,
    #[serde(rename = "type")]
    ty: &'static str,
    square: &'a str,
    author: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pubkey: Option<&'a str>,
    ts: i64,
    merge: serde_json::Value,
    document: &'a serde_json::Value,
    display: String,
    #[serde(rename = "self")]
    is_self: bool,
}

fn message_header<'a>(msg: &'a Message, ty: &'static str) -> MessageHeader<'a> {
    MessageHeader {
        event: "message",
        id: msg.id.as_str(),
        ty,
        square: msg.mesh.as_str(),
        author: msg.author.as_str(),
        pubkey: (!msg.pubkey.is_empty()).then_some(msg.pubkey.as_str()),
        ts: msg.timestamp,
    }
}

/// The pre-formatted, markdown-safe `display` line for a `msg` event —
/// the single source of truth the `/mesh` skill echoes verbatim, so the
/// model never composes or re-types a body. Nicks are wrapped in literal
/// backticks: the skill renders into markdown, where a bare `<nick>` is
/// stripped as an HTML tag, and the code span prevents that. The body is
/// embedded **raw** (never trimmed, escaped, or re-spaced). This is
/// JSON-only — the Human/terminal sink renders separately (ANSI, no
/// backticks).
fn msg_display(author: &str, body: &str, reply: Option<&str>) -> String {
    match reply {
        Some(target) => format!("{MESH_GLYPH}\u{FE0F} `<{author}>` → `<{target}>`: {body}"),
        None => format!("{MESH_GLYPH}\u{FE0F} `<{author}>`: {body}"),
    }
}

/// The value cluster for [`task_display`]: the author/target nicks, the
/// native A2A `kind`/`state`, and the text body.
#[derive(Clone, Copy)]
struct TaskDisplayParams<'a> {
    author: &'a str,
    to: &'a str,
    kind: &'a str,
    state: Option<crate::a2a::TaskState>,
    body: &'a str,
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
    format!("{MESH_GLYPH}\u{FE0F} task {label} `<{author}>` → `<{to}>`: {body}")
}

/// `display` line for a `task_progress` event:
/// `` 💬️ task progress `<author>` → `<to>`: 35/100 `` (or
/// `working` when indeterminate).
fn task_progress_display(author: &str, to: &str, done: Option<u64>, total: Option<u64>) -> String {
    match (done, total) {
        (Some(done), Some(total)) => {
            format!("{MESH_GLYPH}\u{FE0F} task progress `<{author}>` → `<{to}>`: {done}/{total}")
        }
        _ => format!("{MESH_GLYPH}\u{FE0F} task progress `<{author}>` → `<{to}>`: working"),
    }
}

/// `display` line for a presence event: `` 💬️ `<author>` has joined `` /
/// `` 💬️ `<author>` has joined `` / `… has left`. See [`msg_display`] for the
/// backtick rationale.
fn presence_display(author: &str, subtype: PresenceSubtype) -> String {
    format!("{MESH_GLYPH}\u{FE0F} `<{author}>` has {subtype}")
}

/// `display` line for a `peer_timeout` event.
pub(super) fn peer_timeout_display(nickname: &str) -> String {
    format!("{MESH_GLYPH}\u{FE0F} `<{nickname}>` went quiet")
}

/// `display` line for a `peer_return` event.
pub(super) fn peer_return_display(nickname: &str) -> String {
    format!("{MESH_GLYPH}\u{FE0F} `<{nickname}>` came back")
}

/// `display` block for a `ping_report` event: a markdown RTT table (one
/// row per responding peer), or a single line when no peer answered.
pub(super) fn ping_report_display(peers: &[PingPeer], known: usize) -> String {
    if peers.is_empty() {
        return format!("{MESH_GLYPH}\u{FE0F} ping: no peers responded");
    }
    let mut out = format!("{MESH_GLYPH}\u{FE0F} ping\n| peer | RTT |\n|---|---|\n");
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
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{line}");
    let _ = lock.flush();
}

pub(super) fn emit_json<T: Serialize>(value: &T, is_visible: bool) {
    if let Ok(json) = serde_json::to_string(value) {
        emit(&stamp_visibility(json, is_visible));
    }
}

/// Whether an event's `display` line belongs in the agent's transcript — the
/// daemon-owned print decision, stamped on every JSON line as `is_visible` so
/// no skill re-derives a skip-list. Everything else in a poll batch is
/// context, not output: state/meta document echoes (documents are on-demand
/// via `state get`/`meta get`), task legs (interactions the task flow
/// drives), `fork` (a security alert kept for the log/ring, `tracing::warn!`
/// covers debugging), presence `alive` beats, and the operational
/// stream-only events.
pub(crate) fn is_visible(event: &OutputEvent) -> bool {
    match event {
        OutputEvent::Message { .. }
        | OutputEvent::PingReport { .. }
        | OutputEvent::PeerTimeout { .. }
        | OutputEvent::PeerReturn { .. } => true,
        OutputEvent::Presence { msg } => matches!(
            &msg.kind,
            MessageKind::Presence {
                subtype: PresenceSubtype::Joined | PresenceSubtype::Left
            }
        ),
        OutputEvent::Task { .. }
        | OutputEvent::TaskMessage { .. }
        | OutputEvent::TaskTimeout { .. }
        | OutputEvent::StateChanged { .. }
        | OutputEvent::Fork { .. }
        | OutputEvent::Info { .. }
        | OutputEvent::Error { .. }
        | OutputEvent::MsgPosted { .. }
        | OutputEvent::Ready { .. }
        | OutputEvent::MeshId { .. } => false,
    }
}

/// Append `"is_visible":<b>` as the last field of an already-rendered event
/// object. Spliced (like `seq` in [`surfaced_event_json`]) rather than added
/// to every line struct: one stamp point per render path keeps the stream and
/// poll forms byte-identical, and re-parsing to a `Value` would reorder the
/// pinned fields.
fn stamp_visibility(line: String, is_visible: bool) -> String {
    match line.strip_suffix('}') {
        Some(body) => format!("{body},\"is_visible\":{is_visible}}}"),
        None => line,
    }
}

/// Format a presence message as JSON.
///
/// Serializes the struct directly because the documented wire format
/// pins the field order (`event`, `id`, `type`, `mesh`, `author`,
/// `ts`, …) and `Value::to_string` would sort keys alphabetically.
pub(super) fn format_presence_json(msg: &Message, subtype: PresenceSubtype) -> String {
    // Presence carries no body — peer model/harness/host lives in the `meta` channel.
    let line = serde_json::to_string(&PresenceLine {
        header: message_header(msg, "presence"),
        subtype,
        display: presence_display(msg.author.as_str(), subtype),
    })
    .expect("presence event serialization should never fail");
    stamp_visibility(
        line,
        matches!(subtype, PresenceSubtype::Joined | PresenceSubtype::Left),
    )
}

/// Format a chat frame as a JSON string. Presence uses
/// `format_presence_json`; `PeerInfo` is never printed.
pub(super) fn format_msg_json(msg: &Message, is_self: bool) -> String {
    if msg.kind.is_app(crate::a2a::wire::MSG) {
        // Inbound frames were validated at the receive boundary and our
        // own echoes are built by `broadcast_message`, so the parse
        // succeeds in practice; the fallback keeps a display path from
        // ever panicking on a crafted body.
        let payload = serde_json::from_str::<crate::a2a::Message>(msg.body.as_str()).ok();
        let body = payload.as_ref().map_or_else(
            || msg.body.as_str().to_owned(),
            crate::a2a::gossip::display_text,
        );
        stamp_visibility(
            serde_json::to_string(&MsgLine {
                header: message_header(msg, "msg"),
                display: msg_display(msg.author.as_str(), &body, None),
                body,
                to: None,
                message: payload,
                is_self,
            })
            .expect("message event serialization should never fail"),
            true,
        )
    } else {
        unreachable!("format_msg_json only handles chat frames")
    }
}

pub(super) fn print_message_json(msg: &Message, is_self: bool) {
    emit(&format_msg_json(msg, is_self));
}

/// Format a worker-pushed task frame (an `a2a_status` or `a2a_artifact`) as
/// its JSON line. A beat renders as a `task_progress` event; every other leg
/// as a `task` event carrying the native A2A `kind` + `state`, the text
/// projection as `body`, and the whole A2A payload.
pub(super) fn format_task_json(msg: &Message, is_self: bool) -> String {
    let to = match &msg.kind {
        MessageKind::App { tag, to, .. }
            if matches!(
                tag.as_str(),
                crate::a2a::wire::STATUS | crate::a2a::wire::ARTIFACT
            ) =>
        {
            match to {
                Some(to) => to,
                None => unreachable!("a status/artifact frame is always directed"),
            }
        }
        MessageKind::App { .. }
        | MessageKind::Presence { .. }
        | MessageKind::PeerInfo
        | MessageKind::Digest
        | MessageKind::StateDigest
        | MessageKind::MetaDigest
        | MessageKind::Ping
        | MessageKind::Pong { .. }
        | MessageKind::State
        | MessageKind::Meta
        | MessageKind::LinkState => {
            unreachable!("format_task_json only handles status/artifact frames")
        }
    };
    let task_id = crate::a2a::gossip::frame_task_id(msg)
        .expect("a task frame carries its task id")
        .as_str()
        .to_owned();
    // A liveness beat (a status marked `mesh:beat`) → task_progress widget.
    if let Ok(payload) = crate::a2a::gossip::status_payload(msg)
        && crate::a2a::gossip::is_beat(&payload)
    {
        let (done, total) = crate::a2a::gossip::beat_fraction(&payload)
            .map_or((None, None), |(done, total)| (Some(done), Some(total)));
        return stamp_visibility(
            serde_json::to_string(&TaskProgressLine {
                event: "task_progress",
                id: msg.id.as_str(),
                square: msg.mesh.as_str(),
                author: msg.author.as_str(),
                ts: msg.timestamp,
                to: to.as_str(),
                task_id,
                done,
                total,
                display: task_progress_display(msg.author.as_str(), to.as_str(), done, total),
                is_self,
            })
            .expect("task_progress event serialization should never fail"),
            false,
        );
    }
    let kind = crate::a2a::gossip::task_event_kind(msg).unwrap_or("status-update");
    let state = crate::a2a::gossip::frame_task_state(msg);
    let body = crate::a2a::gossip::task_text(msg);
    stamp_visibility(
        serde_json::to_string(&TaskLine {
            event: "task",
            id: msg.id.as_str(),
            square: msg.mesh.as_str(),
            author: msg.author.as_str(),
            pubkey: (!msg.pubkey.is_empty()).then_some(msg.pubkey.as_str()),
            ts: msg.timestamp,
            to: to.as_str(),
            task_id,
            kind,
            state: state.map(crate::a2a::TaskState::as_str),
            display: task_display(TaskDisplayParams {
                author: msg.author.as_str(),
                to: to.as_str(),
                kind,
                state,
                body: &body,
            }),
            body,
            payload: serde_json::from_str(msg.body.as_str()).ok(),
            is_self,
        })
        .expect("task event serialization should never fail"),
        false,
    )
}

/// Format an RPC `message/send` task leg (the initiator's brief / answer /
/// approval, surfaced on the worker; or the created `Task` adopted on the
/// initiator) as a `{"event":"task","kind":"message",...}` line.
pub(super) fn format_task_message_json(leg: &TaskMessageLeg<'_>) -> String {
    stamp_visibility(
        serde_json::to_string(&TaskLine {
            event: "task",
            id: leg.id,
            square: leg.mesh,
            author: leg.author,
            pubkey: None,
            ts: agent_habilis_mesh::util::clock::unix_secs(),
            to: leg.peer,
            task_id: leg.task_id.to_owned(),
            kind: "message",
            state: leg.state.map(crate::a2a::TaskState::as_str),
            display: task_display(TaskDisplayParams {
                author: leg.author,
                to: leg.peer,
                kind: "message",
                state: leg.state,
                body: leg.text,
            }),
            body: leg.text.to_owned(),
            payload: None,
            is_self: leg.is_self,
        })
        .expect("task event serialization should never fail"),
        false,
    )
}

pub(super) fn print_task_json(msg: &Message, is_self: bool) {
    emit(&format_task_json(msg, is_self));
}

/// Make a peer-controlled merge path safe to splice into the `state` display.
/// The path is built from attacker-influenced merge keys, and the display feeds
/// both a markdown renderer and a raw terminal `eprintln`, so strip:
/// - control characters (a `\n` would forge a second line; `MessageBody` permits
///   newlines),
/// - backticks (which would unbalance the code span around the nick), and
/// - the markdown link/image metacharacters `[` `]` `(` `)` (so a path like
///   `[click](https://evil)` can't render as a clickable link),
///
/// and cap the length so one key can't flood the line.
fn sanitize_path(path: &str) -> String {
    path.chars()
        .filter(|ch| !ch.is_control() && !matches!(ch, '`' | '[' | ']' | '(' | ')'))
        .take(80)
        .collect()
}

/// The `changed …` clause for a `state` display, built from a `State` body's
/// already-parsed merge document: the touched paths (sanitized, deduped, capped
/// so the line stays bounded), or `shared state` when none are present. A
/// top-level key with a non-empty object value is descended one level (so
/// `{"peers":{"alice":{…}}}` reads `/peers/alice`, naming the changed entry
/// rather than its every field); anything else names the top-level key. A `null`
/// value (a delete) still counts as touched.
fn state_change_summary(merge: Option<&serde_json::Value>) -> String {
    const MAX: usize = 6;
    let mut paths: Vec<String> = Vec::new();
    let mut push = |raw: String| {
        let clean = sanitize_path(&raw);
        if !clean.is_empty() && !paths.iter().any(|seen| seen == &clean) {
            paths.push(clean);
        }
    };
    if let Some(map) = merge.and_then(serde_json::Value::as_object) {
        for (key, value) in map {
            push_changed_paths(key, value, &mut push);
        }
    }
    if paths.is_empty() {
        "shared state".to_owned()
    } else if paths.len() > MAX {
        format!("{} (+{} more)", paths[..MAX].join(", "), paths.len() - MAX)
    } else {
        paths.join(", ")
    }
}

/// Push the changed path(s) for one top-level `merge` entry. A non-empty
/// object value names its changed members one level deep (`/peers/alice`);
/// anything else names the top-level key.
fn push_changed_paths(key: &str, value: &serde_json::Value, push: &mut impl FnMut(String)) {
    if let serde_json::Value::Object(sub) = value
        && !sub.is_empty()
    {
        for subkey in sub.keys() {
            push(format!("/{key}/{subkey}"));
        }
        return;
    }
    push(format!("/{key}"));
}

/// The surfaced RFC 7386 delta inside a channel-event body: the new `change`
/// form carries it under `m`; a legacy `merge` body under `merge`. `None` when
/// absent (an internal write, or an opaque/unparseable body).
fn body_merge(body: &str) -> Option<serde_json::Value> {
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok()?;
    parsed
        .get("m")
        .or_else(|| parsed.get("merge"))
        .filter(|value| !value.is_null())
        .cloned()
}

/// `display` line for a `state` event: `` 💬️ `<author>` changed /board, /turn ``,
/// or `💬️ you changed …` for your own write (`shared state` when the touched
/// paths aren't known). A peer's nick is backtick-wrapped like every other event
/// so the skill's markdown renderer keeps the `<nick>`; "you" is plain text.
fn state_display(author: &str, is_self: bool, what: &str) -> String {
    if is_self {
        format!("{MESH_GLYPH}\u{FE0F} you changed {what}")
    } else {
        format!("{MESH_GLYPH}\u{FE0F} `<{author}>` changed {what}")
    }
}

/// Render a `StateChanged` event as its `{"event":"state",...}` JSON line: the
/// header, the merge delta (pulled out of the `State` body), the freshly-derived
/// `document`, the `display` line, and `self`.
pub(super) fn format_state_json(
    channel: agent_habilis_mesh::protocol::Channel,
    event: &Message,
    document: &serde_json::Value,
    is_self: bool,
) -> String {
    // The surfaced delta feeds both the `merge` field and the touched-paths
    // `display` summary. It rides the change body under `m` (a legacy `merge`
    // body under `merge`); an internal write carries none.
    let merge = body_merge(event.body.as_str());
    let what = state_change_summary(merge.as_ref());
    stamp_visibility(
        serde_json::to_string(&StateLine {
            event: channel.label(),
            id: event.id.as_str(),
            ty: channel.label(),
            square: event.mesh.as_str(),
            author: event.author.as_str(),
            pubkey: (!event.pubkey.is_empty()).then_some(event.pubkey.as_str()),
            ts: event.timestamp,
            merge: merge.unwrap_or(serde_json::Value::Null),
            document,
            display: state_display(event.author.as_str(), is_self, &what),
            is_self,
        })
        .expect("state event serialization should never fail"),
        false,
    )
}

/// Render a `seq`-tagged surfaced event to the exact stream JSON line, with the
/// daemon-local `seq` flattened in as a leading field so a `poll` client can
/// advance its `--after` cursor. The body after `seq` is byte-identical to the
/// live `--output json` line for the same event (same [`event_json`]
/// renderer) — the parity guarantee `poll` rests on. `None` for events that
/// produce no JSON line (e.g. `MeshId`).
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

/// Render a captured [`OutputEvent`] to the exact JSON line the
/// daemon writes in `--output json` mode. Reuses the same serializers
/// as the `Stream` sink, so in-process tests assert the byte-identical
/// wire format the `/mesh` skill + MCP clients parse. `None` for events
/// that produce no JSON line in JSON mode (`MeshId` is the bare stderr
/// `💬…` line, never JSON).
#[must_use]
pub fn event_json(event: &OutputEvent) -> Option<String> {
    let json = match event {
        OutputEvent::Ready {
            mesh,
            name,
            nickname,
            drift,
            a2a_port,
        } => serde_json::to_string(&SimpleEvent::Ready {
            version: crate::VERSION,
            square: mesh.as_str(),
            name: name.as_str(),
            nickname: nickname.as_str(),
            drift: drift.as_deref(),
            a2a_port: *a2a_port,
        }),
        OutputEvent::Message { msg, is_self } => return Some(format_msg_json(msg, *is_self)),
        OutputEvent::Task { msg, is_self } => {
            return Some(format_task_json(msg, *is_self));
        }
        OutputEvent::TaskMessage {
            id,
            mesh,
            author,
            peer,
            task_id,
            state,
            text,
            is_self,
        } => {
            return Some(format_task_message_json(&TaskMessageLeg {
                id,
                mesh,
                author,
                peer,
                task_id,
                state: *state,
                text,
                is_self: *is_self,
            }));
        }
        OutputEvent::TaskTimeout { task_id } => serde_json::to_string(&SimpleEvent::TaskTimeout {
            task_id: task_id.as_str(),
        }),
        OutputEvent::Presence { msg } => {
            let MessageKind::Presence { subtype } = &msg.kind else {
                return None;
            };
            return Some(format_presence_json(msg, *subtype));
        }
        OutputEvent::PeerTimeout {
            nickname,
            last_seen_secs_ago,
        } => serde_json::to_string(&SimpleEvent::PeerTimeout {
            nickname: nickname.as_str(),
            last_seen_secs_ago: *last_seen_secs_ago,
            display: peer_timeout_display(nickname.as_str()),
        }),
        OutputEvent::PeerReturn { nickname } => serde_json::to_string(&SimpleEvent::PeerReturn {
            nickname: nickname.as_str(),
            display: peer_return_display(nickname.as_str()),
        }),
        OutputEvent::Fork {
            nickname,
            pubkey,
            seq,
        } => serde_json::to_string(&SimpleEvent::Fork {
            nickname: nickname.as_str(),
            pubkey,
            seq: *seq,
        }),
        OutputEvent::MsgPosted { id } => {
            serde_json::to_string(&SimpleEvent::MsgPosted { id: id.as_str() })
        }
        OutputEvent::Info { message } => serde_json::to_string(&SimpleEvent::Info { message }),
        OutputEvent::Error { message } => serde_json::to_string(&SimpleEvent::Error { message }),
        OutputEvent::PingReport { peers, known } => {
            serde_json::to_string(&SimpleEvent::PingReport {
                responded: peers.len(),
                display: ping_report_display(peers, *known),
                peers: peers.clone(),
                known: *known,
            })
        }
        OutputEvent::StateChanged {
            channel,
            event,
            document,
            is_self,
        } => return Some(format_state_json(*channel, event, document, *is_self)),
        OutputEvent::MeshId { .. } => return None,
    };
    json.ok()
        .map(|line| stamp_visibility(line, is_visible(event)))
}
