//! Output-module tests: the JSON wire format (shape assertions + insta
//! snapshots + proptests). The wire serializers under test live in
//! [`super::simple_event`].

use habilis_network::protocol::message::{Authored, Verified};
use habilis_network::protocol::{Message, MessageKind, PresenceSubtype};

use super::simple_event::{SimpleEvent, format_msg_json, format_presence_json};
use crate::a2a::{node, status_update_params, wire};
use crate::output::simple_event;

fn parse(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|error| panic!("invalid JSON: {error}\n{text}"))
}

// ── format_msg_json: msg type ──────────────────────────────

fn nick(name: &str) -> habilis_network::protocol::Nickname {
    habilis_network::protocol::Nickname::from(name)
}

fn sid() -> habilis_network::protocol::MeshId {
    habilis_network::protocol::MeshId::from("test")
}

/// A chat frame carrying a real A2A payload, its frame id pinned to the
/// payload's `messageId` — the invariant `broadcast_message` establishes.
fn view(message: Message) -> Authored {
    Verified::fixture_unchecked(
        message,
        habilis_network::protocol::AuthorKey::from_bytes([0; 32]),
    )
    .authored()
}

fn chat_frame_raw(author: &str, text: &str) -> Message {
    let payload = status_update_params::compose_broadcast(&sid(), text);
    let frame_id = habilis_network::protocol::MessageId::new(payload.message_id.as_str())
        .expect("an a2a id is a valid frame id");
    let body = status_update_params::payload_body(&payload).expect("payload serializes");
    Message::new_app(
        &sid(),
        &nick(author),
        habilis_network::protocol::AppFrameParams {
            tag: habilis_network::protocol::AppTag::from(wire::BROADCAST),
            to: None,
            corr: None,
            body,
        },
    )
    .with_id(frame_id)
}

fn chat_frame(author: &str, text: &str) -> Authored {
    view(chat_frame_raw(author, text))
}

#[test]
fn json_message_has_all_fields() {
    let message = chat_frame("alice", "hello");
    let json = format_msg_json(&message, false);
    let parsed = parse(&json);
    assert_eq!(parsed["event"], "message", "the event field is message");
    assert_eq!(parsed["type"], "broadcast", "the type field is broadcast");
    assert_eq!(
        parsed["gossip"],
        sid().as_str(),
        "the gossip field is sid().as_str()"
    );
    assert_eq!(parsed["author"], "alice", "the author field is alice");
    assert_eq!(parsed["body"], "hello", "the body field is hello");
    assert!(parsed["to"].is_null(), "the to field is null");
    assert_eq!(parsed["self"], false, "the self field is false");
    assert!(parsed["id"].is_string(), "the id field is a string");
    assert!(parsed["ts"].is_number(), "the ts field is a number");
    // The embedded A2A object rides along whole (v1.0 ProtoJSON: no inline
    // `kind`, `ROLE_USER`), its id the frame's id.
    assert_eq!(
        parsed["message"]["messageId"], parsed["id"],
        "the embedded A2A messageId is the frame id"
    );
    assert_eq!(
        parsed["message"]["role"], "ROLE_USER",
        "the embedded A2A role stays ROLE_USER on the wire"
    );
    assert_eq!(
        parsed["message"]["parts"][0]["text"], "hello",
        "the embedded message keeps its text part unchanged"
    );
    // An author view always has a key: the fixture's zero key here, never an omitted field.
    assert_eq!(
        parsed["pubkey"],
        "0".repeat(64),
        "the pubkey field is 0.repeat(64)"
    );
}

#[test]
fn json_signed_message_exposes_full_pubkey() {
    // A real (signed) message carries the author's full Ed25519 public key
    // (hex) in the JSON event — the identity behind the display nickname.
    let identity = habilis_network::protocol::Identity::generate();
    let message = chat_frame_raw("alice", "hi").sign(&identity).authored();
    let parsed = parse(&format_msg_json(&message, false));
    let pubkey = parsed["pubkey"].as_str().expect("signed event has pubkey");
    assert_eq!(
        pubkey,
        message.key().to_string(),
        "event pubkey must be the message's key"
    );
    assert_eq!(pubkey.len(), 64, "full 32-byte key as hex");
}

#[test]
fn ready_event_drift_is_present_only_when_stale() {
    use super::OutputEvent;
    let make = |drift: Option<&str>| {
        simple_event::event_json(&OutputEvent::Ready {
            mesh: sid(),
            name: habilis_network::protocol::MeshName::new("team").unwrap(),
            nickname: nick("alice"),
            drift: drift.map(str::to_owned),
            a2a_port: None,
        })
        .expect("ready event produces a JSON line")
    };

    // In sync: no `drift` key on the wire.
    let clean = parse(&make(None));
    assert_eq!(clean["event"], "ready", "the event field is ready");
    assert!(clean.get("drift").is_none(), "the drift key is absent");

    // Stale: the warning rides along verbatim.
    let stale = parse(&make(Some("💬️ run `agent-gossip plug` to update skills")));
    assert_eq!(
        stale["drift"], "💬️ run `agent-gossip plug` to update skills",
        "a stale-skill warning reaches the drift field verbatim"
    );
}

#[test]
fn fork_event_json_shape() {
    use super::OutputEvent;
    let pubkey = "deadbeef".repeat(8); // 64 hex
    let json = simple_event::event_json(&OutputEvent::Fork {
        nickname: nick("alice"),
        pubkey: pubkey.clone(),
        seq: 7,
    })
    .expect("fork event produces a JSON line");
    let parsed = parse(&json);
    assert_eq!(parsed["event"], "fork", "the event field is fork");
    assert_eq!(parsed["nickname"], "alice", "the nickname field is alice");
    assert_eq!(parsed["pubkey"], pubkey, "the pubkey field is pubkey");
    assert_eq!(parsed["seq"], 7, "the seq field is 7");
}

#[test]
fn json_self_flag_true() {
    let message = chat_frame("alice", "hi");
    let parsed = parse(&format_msg_json(&message, true));
    assert_eq!(parsed["self"], true, "the self field is true");
}

#[test]
fn json_body_escapes_quotes() {
    let message = chat_frame("alice", r#"say "hello""#);
    let json = format_msg_json(&message, false);
    let parsed = parse(&json);
    assert_eq!(
        parsed["body"], r#"say "hello""#,
        "the body field is r#say hello#"
    );
}

#[test]
fn json_body_escapes_backslashes() {
    let message = chat_frame("alice", r"path\to\file");
    let json = format_msg_json(&message, false);
    let parsed = parse(&json);
    assert_eq!(
        parsed["body"], r"path\to\file",
        "the body field is rpath\\to\\file"
    );
}

// ── format_msg_json: presence type ─────────────────────────

#[test]
fn json_presence_joined() {
    let message = view(Message::new_joined(&sid(), &nick("alice")));
    let parsed = parse(&format_presence_json(&message, PresenceSubtype::Joined));
    assert_eq!(parsed["event"], "message", "the event field is message");
    assert_eq!(parsed["type"], "presence", "the type field is presence");
    assert_eq!(parsed["subtype"], "joined", "the subtype field is joined");
    assert_eq!(parsed["author"], "alice", "the author field is alice");
    assert_eq!(
        parsed["gossip"],
        sid().as_str(),
        "the gossip field is sid().as_str()"
    );
    assert_eq!(
        parsed["display"], "💬️ `<alice>` has joined",
        "the display field is 💬️ `<alice>` has joined"
    );
}

#[test]
fn json_presence_left() {
    let message = view(Message::new_left(&sid(), &nick("bob")));
    let parsed = parse(&format_presence_json(&message, PresenceSubtype::Left));
    assert_eq!(parsed["subtype"], "left", "the subtype field is left");
    assert_eq!(parsed["author"], "bob", "the author field is bob");
}

// ── task type ──────────────────────────────────────────────

/// The value cluster for the `status_frame` test helper: the crafted
/// frame's task id (with optional beat metadata).
struct StatusFrameArgs<'caller> {
    task_id: &'caller str,
    state: crate::a2a::TaskState,
    note: Option<&'caller str>,
    metadata: Option<serde_json::Value>,
}

/// A task **status** frame carrying `state` (with optional beat metadata).
fn status_frame_raw(author: &str, to: &str, args: StatusFrameArgs<'_>) -> Message {
    let StatusFrameArgs {
        task_id,
        state,
        note,
        metadata,
    } = args;
    let tid = crate::a2a::TaskId::from_uuid_str(task_id).expect("valid task id");
    let update = status_update_params::status_update(
        &sid(),
        status_update_params::StatusUpdateParams {
            task_id: &tid,
            state,
            note,
            metadata,
        },
    );
    let body = status_update_params::payload_body(&update).expect("payload serializes");
    let kind = MessageKind::app_to(wire::STATUS, nick(to), None);
    Message::new_frame(&sid(), &nick(author), kind, body)
}

fn status_frame(author: &str, to: &str, args: StatusFrameArgs<'_>) -> Authored {
    view(status_frame_raw(author, to, args))
}

#[test]
fn task_event_json_shape() {
    use super::OutputEvent;
    let message = status_frame(
        "drift-oak",
        "calm-otter",
        StatusFrameArgs {
            task_id: "550e8400-e29b-41d4-a716-446655440000",
            state: crate::a2a::TaskState::Working,
            note: Some("on it"),
            metadata: None,
        },
    );
    // Routed through `event_json` so the captured-event form is
    // byte-identical to the stdout `--output json` line.
    let json = simple_event::event_json(&OutputEvent::Task {
        message: Box::new(message),
        is_self: false,
    })
    .expect("task event produces a JSON line");
    let parsed = parse(&json);
    assert_eq!(parsed["event"], "task", "the event field is task");
    assert_eq!(
        parsed["author"], "drift-oak",
        "the author field is drift-oak"
    );
    assert_eq!(parsed["to"], "calm-otter", "the to field is calm-otter");
    assert_eq!(
        parsed["task_id"], "550e8400-e29b-41d4-a716-446655440000",
        "the task_id field is 550e8400-e29b-41d4-a716-446655440000"
    );
    assert_eq!(
        parsed["kind"], "status-update",
        "the kind field is status-update"
    );
    assert_eq!(parsed["state"], "working", "the state field is working");
    assert_eq!(parsed["self"], false, "the self field is false");
    assert!(parsed["id"].is_string(), "the id field is a string");
    assert!(parsed["ts"].is_number(), "the ts field is a number");
    // The A2A construct rides along whole for A2A-aware consumers (v1.0
    // ProtoJSON: no inline `kind`, `TASK_STATE_*`).
    assert_eq!(
        parsed["payload"]["status"]["state"], "TASK_STATE_WORKING",
        "a working status keeps the v1.0 TASK_STATE_WORKING name"
    );
    assert!(parsed.get("phase").is_none(), "the phase key is absent");
    // Distinct top-level event — never the `message` family, no `type` key.
    assert!(parsed.get("type").is_none(), "the type key is absent");
}

#[test]
fn task_self_echo_flag() {
    use super::OutputEvent;
    let message = status_frame(
        "drift-oak",
        "calm-otter",
        StatusFrameArgs {
            task_id: "550e8400-e29b-41d4-a716-446655440000",
            state: crate::a2a::TaskState::Completed,
            note: Some("looks good"),
            metadata: None,
        },
    );
    let json = simple_event::event_json(&OutputEvent::Task {
        message: Box::new(message),
        is_self: true,
    })
    .expect("task event produces a JSON line");
    let parsed = parse(&json);
    assert_eq!(parsed["self"], true, "the self field is true");
    assert_eq!(
        parsed["kind"], "status-update",
        "the kind field is status-update"
    );
    assert_eq!(parsed["state"], "completed", "the state field is completed");
    assert_eq!(
        parsed["payload"]["status"]["state"], "TASK_STATE_COMPLETED",
        "a completed status keeps the v1.0 TASK_STATE_COMPLETED name"
    );
}

#[test]
fn task_event_carries_the_waiting_reason_only_when_the_leg_has_one() {
    use super::OutputEvent;
    let line = |metadata| {
        let message = status_frame(
            "calm-otter",
            "drift-oak",
            StatusFrameArgs {
                task_id: "550e8400-e29b-41d4-a716-446655440000",
                state: crate::a2a::TaskState::Working,
                note: None,
                metadata,
            },
        );
        let json = simple_event::event_json(&OutputEvent::Task {
            message: Box::new(message),
            is_self: false,
        })
        .expect("task event produces a JSON line");
        parse(&json)
    };
    let waiting = line(status_update_params::waiting_metadata(Some(
        "asks its user",
    )));
    assert_eq!(waiting["event"], "task", "the event field is task");
    assert_eq!(
        waiting["waiting"], "asks its user",
        "the waiting field is asks its user"
    );
    let plain = line(None);
    assert!(plain.get("waiting").is_none(), "got: {plain}");
}

/// The record keeps the reason from a `working` leg only, so the event key
/// follows: a peer that sends `input-required` with `mesh:waiting` must not
/// make the initiator badge a task that is not working.
#[test]
fn task_event_has_no_waiting_key_on_a_leg_that_is_not_working() {
    use super::OutputEvent;
    let message = status_frame(
        "calm-otter",
        "drift-oak",
        StatusFrameArgs {
            task_id: "550e8400-e29b-41d4-a716-446655440000",
            state: crate::a2a::TaskState::InputRequired,
            note: None,
            metadata: status_update_params::waiting_metadata(Some("asks its user")),
        },
    );
    let json = simple_event::event_json(&OutputEvent::Task {
        message: Box::new(message),
        is_self: false,
    })
    .expect("task event produces a JSON line");
    let parsed = parse(&json);
    assert_eq!(
        parsed["state"], "input-required",
        "the state field is input-required"
    );
    assert!(parsed.get("waiting").is_none(), "got: {parsed}");
}

#[test]
fn task_progress_event_json_shape() {
    use super::OutputEvent;
    let message = status_frame(
        "calm-otter",
        "drift-oak",
        StatusFrameArgs {
            task_id: "550e8400-e29b-41d4-a716-446655440000",
            state: crate::a2a::TaskState::Working,
            note: None,
            metadata: Some(status_update_params::beat_metadata(Some((35, 100)))),
        },
    );
    let json = simple_event::event_json(&OutputEvent::Task {
        message: Box::new(message),
        is_self: false,
    })
    .expect("progress event produces a JSON line");
    let parsed = parse(&json);
    // A beat renders as a distinct `task_progress` event with its
    // `done`/`total`, never as a content `task` event.
    assert_eq!(
        parsed["event"], "task_progress",
        "the event field is task_progress"
    );
    assert_eq!(
        parsed["task_id"], "550e8400-e29b-41d4-a716-446655440000",
        "the task_id field is 550e8400-e29b-41d4-a716-446655440000"
    );
    assert_eq!(parsed["done"], 35, "the done field is 35");
    assert_eq!(parsed["total"], 100, "the total field is 100");
    assert!(parsed.get("phase").is_none(), "the phase key is absent");
    assert_eq!(
        parsed["display"], "💬️ task progress `<calm-otter>` → `<drift-oak>`: 35/100",
        "the progress line names both peers and the 35 of 100 fraction"
    );
}

#[test]
fn task_timeout_event_json_shape() {
    use super::{OutputEvent, TaskGoneReason};
    use crate::a2a::TaskId;
    let json = simple_event::event_json(&OutputEvent::TaskTimeout {
        task_id: TaskId::from("550e8400-e29b-41d4-a716-446655440000"),
        reason: TaskGoneReason::Timeout,
    })
    .expect("task_timeout event produces a JSON line");
    let parsed = parse(&json);
    assert_eq!(
        parsed["event"], "task_timeout",
        "the event field is task_timeout"
    );
    assert_eq!(
        parsed["task_id"], "550e8400-e29b-41d4-a716-446655440000",
        "the task_id field is 550e8400-e29b-41d4-a716-446655440000"
    );
    assert_eq!(parsed["reason"], "timeout", "the reason field is timeout");
}

#[test]
fn task_peer_left_event_json_shape() {
    use super::{OutputEvent, TaskGoneReason};
    use crate::a2a::TaskId;
    let json = simple_event::event_json(&OutputEvent::TaskTimeout {
        task_id: TaskId::from("550e8400-e29b-41d4-a716-446655440000"),
        reason: TaskGoneReason::PeerLeft,
    })
    .expect("task_timeout event produces a JSON line");
    let parsed = parse(&json);
    assert_eq!(
        parsed["event"], "task_timeout",
        "the event field is task_timeout"
    );
    assert_eq!(
        parsed["reason"], "peer-left",
        "the reason field is peer-left"
    );
}

#[test]
fn task_stalled_event_json_shape() {
    use super::OutputEvent;
    use crate::a2a::TaskId;
    let event = OutputEvent::TaskStalled {
        task_id: TaskId::from("550e8400-e29b-41d4-a716-446655440000"),
        peer: habilis_network::protocol::Nickname::from("calm-otter"),
        last_poll_secs_ago: 2_400,
        check_secs: 1_800,
    };
    let json = simple_event::event_json(&event).expect("task_stalled event produces a JSON line");
    let parsed = parse(&json);
    assert_eq!(
        parsed["event"], "task_stalled",
        "the event field is task_stalled"
    );
    assert_eq!(
        parsed["task_id"], "550e8400-e29b-41d4-a716-446655440000",
        "the task_id field is 550e8400-e29b-41d4-a716-446655440000"
    );
    assert_eq!(parsed["peer"], "calm-otter", "the peer field is calm-otter");
    assert_eq!(
        parsed["last_poll_secs_ago"], 2_400,
        "the last_poll_secs_ago field is 2_400"
    );
    assert_eq!(parsed["check_secs"], 1_800, "the check_secs field is 1_800");
    assert_eq!(
        parsed["display"], "💬️ task stalled · `<calm-otter>` has not polled for 40 min",
        "the stalled line names the silent peer and its 40 minutes"
    );
    assert!(simple_event::is_visible(&event), "the user must see it");
    assert!(node::wakes(&event), "it must ring the bell");
    assert!(node::is_pollable(&event), "poll must serve it");
}

// ── peer_timeout / peer_return shape ───────────────────────

#[test]
fn peer_timeout_json_shape() {
    let json = serde_json::to_string(&SimpleEvent::PeerTimeout {
        nickname: "ball-blue",
        last_seen_secs_ago: 94,
        display: "💬️ `<ball-blue>` went quiet".to_owned(),
    })
    .unwrap();
    let parsed = parse(&json);
    assert_eq!(
        parsed["event"], "peer_timeout",
        "the event field is peer_timeout"
    );
    assert_eq!(
        parsed["nickname"], "ball-blue",
        "the nickname field is ball-blue"
    );
    assert_eq!(
        parsed["last_seen_secs_ago"], 94,
        "the last_seen_secs_ago field is 94"
    );
    assert_eq!(
        parsed["display"], "💬️ `<ball-blue>` went quiet",
        "the display field is 💬️ `<ball-blue>` went quiet"
    );
}

#[test]
fn peer_return_json_shape() {
    let json = serde_json::to_string(&SimpleEvent::PeerReturn {
        nickname: "ball-blue",
        display: "💬️ `<ball-blue>` came back".to_owned(),
    })
    .unwrap();
    let parsed = parse(&json);
    assert_eq!(
        parsed["event"], "peer_return",
        "the event field is peer_return"
    );
    assert_eq!(
        parsed["nickname"], "ball-blue",
        "the nickname field is ball-blue"
    );
    assert_eq!(
        parsed["display"], "💬️ `<ball-blue>` came back",
        "the display field is 💬️ `<ball-blue>` came back"
    );
}

#[test]
fn receipt_json_shape() {
    let json = serde_json::to_string(&SimpleEvent::Receipt {
        from: "ball-blue",
        ref_id: "7a1c",
        state: "delivered",
        ts: 1_792_000_000,
    })
    .unwrap();
    assert_eq!(
        json,
        r#"{"event":"receipt","from":"ball-blue","ref":"7a1c","state":"delivered","ts":1792000000}"#,
        "a delivered receipt is written as this exact one-line JSON"
    );
}

#[test]
fn receipt_event_is_invisible_and_stamped_so() {
    let event = crate::output::OutputEvent::Receipt {
        from: nick("ball-blue"),
        ref_id: "7a1c".to_owned(),
        state: "read".to_owned(),
        ts: 1,
    };
    assert!(
        !simple_event::is_visible(&event),
        "the event is not visible"
    );
    let line = crate::output::event_json(&event).unwrap();
    assert_eq!(
        parse(&line)["is_visible"],
        false,
        "the event is hidden from the user"
    );
    assert_eq!(parse(&line)["state"], "read", "the receipt state is read");
}

// ── all outputs are valid single-line JSON ─────────────────────

#[test]
fn json_output_is_single_line() {
    let msgs: Vec<String> = vec![
        format_msg_json(&chat_frame("alice", "hello world"), false),
        format_presence_json(
            &view(Message::new_joined(&sid(), &nick("charlie"))),
            PresenceSubtype::Joined,
        ),
        format_presence_json(
            &view(Message::new_left(&sid(), &nick("dave"))),
            PresenceSubtype::Left,
        ),
    ];
    for json in msgs {
        assert!(!json.contains('\n'), "JSON should be single line: {json}");
        assert!(parse(&json).is_object(), "the line parses to a json object");
    }
}

mod snapshots {
    use habilis_network::protocol::message::Authored;
    use habilis_network::protocol::{Message, MessageKind, PresenceSubtype};

    use super::{format_msg_json, format_presence_json, view};
    use crate::a2a::{status_update_params, wire};

    /// Deterministic task frames for the wire-pinned snapshots: ids fixed,
    /// timestamps from the fixture.
    fn snap_frame_raw(kind: MessageKind, body: &str) -> Message {
        Message::fixture(kind, body)
    }

    fn snap_frame(kind: MessageKind, body: &str) -> Authored {
        view(snap_frame_raw(kind, body))
    }

    const SNAP_TASK_ID: &str = "550e8400-e29b-41d4-a716-446655440000";

    fn snap_status(
        state: crate::a2a::TaskState,
        note: Option<&str>,
        metadata: Option<serde_json::Value>,
    ) -> String {
        let tid = crate::a2a::TaskId::from(SNAP_TASK_ID);
        let mut update = status_update_params::status_update(
            &habilis_network::protocol::MeshId::from("test"),
            status_update_params::StatusUpdateParams {
                task_id: &tid,
                state,
                note,
                metadata,
            },
        );
        // Pin the note message's random id for a stable snapshot.
        if let Some(message) = update.status.message.as_mut() {
            message.message_id =
                crate::a2a::MessageId::from("00000000-0000-0000-0000-000000000002");
        }
        let body = status_update_params::payload_body(&update).expect("payload serializes");
        let frame = snap_frame(
            MessageKind::app_to(
                wire::STATUS,
                habilis_network::protocol::Nickname::from("calm-otter"),
                None,
            ),
            body.as_str(),
        );
        super::super::simple_event::format_task_json(&frame, false)
    }

    #[test]
    fn snap_task_confirm() {
        insta::assert_snapshot!(snap_status(
            crate::a2a::TaskState::Completed,
            Some("looks good"),
            None,
        ));
    }

    #[test]
    fn snap_task_progress() {
        insta::assert_snapshot!(snap_status(
            crate::a2a::TaskState::Working,
            None,
            Some(status_update_params::beat_metadata(Some((35, 100)))),
        ));
    }

    #[test]
    fn snap_task_done() {
        let tid = crate::a2a::TaskId::from(SNAP_TASK_ID);
        let mut update = status_update_params::artifact_update(
            &habilis_network::protocol::MeshId::from("test"),
            &tid,
            "2 findings: missing await in recv; unbounded buffer in flush",
        );
        update.artifact.artifact_id = "00000000-0000-0000-0000-00000000000a".to_string();
        let body = status_update_params::payload_body(&update).expect("payload serializes");
        let frame = snap_frame(
            MessageKind::app_to(
                wire::ARTIFACT,
                habilis_network::protocol::Nickname::from("calm-otter"),
                None,
            ),
            body.as_str(),
        );
        insta::assert_snapshot!(super::super::simple_event::format_task_json(&frame, false));
    }

    /// A deterministic chat frame for snapshots: the payload's random
    /// `messageId` is pinned to the fixture id so the snapshot is stable and
    /// the frame keeps the id == messageId invariant.
    fn snap_chat_frame_raw(text: &str) -> Message {
        let mut payload = status_update_params::compose_broadcast(
            &habilis_network::protocol::MeshId::from("test"),
            text,
        );
        payload.message_id = crate::a2a::MessageId::from("00000000-0000-0000-0000-000000000001");
        let body = status_update_params::payload_body(&payload).expect("payload serializes");
        Message::fixture(MessageKind::app_broadcast(wire::BROADCAST), body.as_str())
    }

    fn snap_chat_frame(text: &str) -> Authored {
        view(snap_chat_frame_raw(text))
    }

    /// The directed twin of [`snap_chat_frame`].
    fn snap_msg_frame_raw(text: &str) -> Message {
        let mut payload = status_update_params::compose_msg(
            &habilis_network::protocol::MeshId::from("test"),
            text,
        );
        payload.message_id = crate::a2a::MessageId::from("00000000-0000-0000-0000-000000000001");
        let body = status_update_params::payload_body(&payload).expect("payload serializes");
        Message::fixture(
            MessageKind::app_to(
                wire::MSG,
                habilis_network::protocol::Nickname::from("addressed-nick"),
                None,
            ),
            body.as_str(),
        )
    }

    fn snap_msg_frame(text: &str) -> Authored {
        view(snap_msg_frame_raw(text))
    }

    #[test]
    fn snap_broadcast_message() {
        insta::assert_snapshot!(format_msg_json(&snap_chat_frame("What is Rust?"), false));
    }

    /// Pins the two fields that separate a private line from a public one:
    /// `type: "msg"` with a non-null `to`, and the `→` in `display`.
    #[test]
    fn snap_msg_message() {
        insta::assert_snapshot!(format_msg_json(
            &snap_msg_frame("Rust is a systems language."),
            false
        ));
    }

    #[test]
    fn snap_presence_joined() {
        let message = view(Message::fixture(
            MessageKind::Presence {
                subtype: PresenceSubtype::Joined,
            },
            "",
        ));
        insta::assert_snapshot!(format_presence_json(&message, PresenceSubtype::Joined));
    }

    #[test]
    fn snap_presence_left() {
        let message = view(Message::fixture(
            MessageKind::Presence {
                subtype: PresenceSubtype::Left,
            },
            "",
        ));
        insta::assert_snapshot!(format_presence_json(&message, PresenceSubtype::Left));
    }

    /// The `{"event":"state",…}` line a shared-state change emits on the
    /// `--output json` stream: header + the applied `merge` document + the
    /// newly-derived `document` + `display` + `self`. Field order is wire-pinned.
    fn snap_state(merge: &str, document: &serde_json::Value, is_self: bool) -> String {
        let body = format!(r#"{{"k":"merge","merge":{merge}}}"#);
        super::super::simple_event::format_state_json(
            habilis_network::protocol::Channel::State,
            &view(Message::fixture(MessageKind::State, &body)),
            document,
            is_self,
        )
    }

    /// Every peer's agent card, the shape a `meta` document holds.
    fn four_card_meta_document() -> serde_json::Value {
        let card = serde_json::json!({
            "name": "peer",
            "description": "agent-gossip peer — an AI agent reachable over the mesh's A2A gossip binding",
            "version": "0.9.0 (1f15293 dirty:false)",
            "capabilities": {"streaming": true, "extensions": [
                {"uri": "https://agent-habilis.dev/a2a/ext/mesh-broadcast/v1", "description": "gossip-wide broadcast Messages"},
                {"uri": "https://agent-habilis.dev/a2a/ext/mesh-state/v1", "description": "a shared RFC 7386 JSON document per gossip"},
                {"uri": "https://agent-habilis.dev/a2a/ext/mesh-a2a-rpc/v1", "description": "serves A2A over gossip"},
                {"uri": "https://agent-habilis.dev/a2a/ext/mesh-seal/v1", "params": {"x25519": "C1MW9JGaHdWscBb7Kwa5Vo8o8zsdURZP4TdMsc6xwLNP"}}
            ]},
            "skills": [{"id": "chat", "name": "chat"}, {"id": "delegate", "name": "delegate"}]
        });
        let peers: serde_json::Map<String, serde_json::Value> = ["aa", "bb", "cc", "dd"]
            .iter()
            .map(|nick| {
                (
                    (*nick).to_owned(),
                    serde_json::json!({"card": card, "model": "m", "harness": "h", "status": "idle"}),
                )
            })
            .collect();
        serde_json::json!({ "peers": peers })
    }

    /// A `meta` event drops the derived document: it is every peer's agent
    /// card, ~4k tokens with four peers, and the skills read it on demand with
    /// `meta get`. The merge and the display stay. Pre-fix every meta echo
    /// repeated all cards, and polls after a join reached 30 KB.
    #[test]
    fn a_meta_event_carries_no_document() {
        let merge = serde_json::json!({"peers": {"aa": {"status": "busy"}}});
        let message = view(Message::fixture(
            MessageKind::State,
            &format!(r#"{{"k":"merge","merge":{merge}}}"#),
        ));
        let document = four_card_meta_document();
        let line = super::super::simple_event::format_state_json(
            habilis_network::protocol::Channel::Meta,
            &message,
            &document,
            false,
        );
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(
            parsed.get("document").is_none(),
            "meta event carries the document: {line}"
        );
        assert_eq!(parsed["merge"], merge, "the merge field is merge");
        assert!(
            parsed["display"].as_str().unwrap().contains("/peers/aa"),
            "the display names the peer path"
        );
    }

    /// Pin: a `state` event keeps its document; the MCP tools promise it.
    #[test]
    fn a_state_event_keeps_its_document() {
        let merge = serde_json::json!({"turn": "b"});
        let message = view(Message::fixture(
            MessageKind::State,
            &format!(r#"{{"k":"merge","merge":{merge}}}"#),
        ));
        let document = serde_json::json!({"turn": "b"});
        let line = super::super::simple_event::format_state_json(
            habilis_network::protocol::Channel::State,
            &message,
            &document,
            false,
        );
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            parsed["document"], document,
            "the document field is document"
        );
    }

    #[test]
    fn snap_state_change() {
        insta::assert_snapshot!(snap_state(
            r#"{"turn": "b"}"#,
            &serde_json::json!({"turn": "b", "n": 1}),
            false,
        ));
    }

    #[test]
    fn state_display_sanitizes_peer_controlled_paths() {
        // A peer-crafted merge key with a backtick (would break the markdown code
        // span around the nick) and a newline (would forge a second line in the
        // human eprintln feed) must not reach the `display` verbatim.
        let line = snap_state("{\"x`\\n<evil>\": 1}", &serde_json::json!({}), false);
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        let display = parsed["display"].as_str().unwrap();
        assert!(!display.contains('\n'), "no newline injection: {display:?}");
        assert_eq!(
            display.matches('`').count(),
            2,
            "only the two nick-wrapping backticks remain: {display:?}"
        );
        assert!(display.contains("/x<evil>"), "sanitized path: {display:?}");
    }

    #[test]
    fn snap_state_self() {
        insta::assert_snapshot!(snap_state(
            r#"{"n": 1}"#,
            &serde_json::json!({"n": 1}),
            true,
        ));
    }
}

mod prop {
    use habilis_network::protocol::Message;
    use proptest::collection::vec as arb_vec;
    use proptest::prelude::any;
    use proptest::strategy::Strategy;
    use proptest::{prop_assert, prop_assert_eq, proptest};

    use super::{PresenceSubtype, format_msg_json, format_presence_json, sid, view};

    fn arb_ascii_body() -> impl Strategy<Value = String> {
        arb_vec(0x20u8..0x7eu8, 0..200).prop_map(|bytes| String::from_utf8(bytes).unwrap())
    }

    fn arb_nickname() -> impl Strategy<Value = habilis_network::protocol::Nickname> {
        "[a-z]{3,8}-[a-z]{3,8}"
            .prop_map(|raw| habilis_network::protocol::Nickname::new(raw).unwrap())
    }

    proptest! {
        #![proptest_config(crate::proptest_support::config())]
        #[test]
        fn prop_message_json_is_valid(
            body_text in arb_ascii_body(),
            author in arb_nickname(),
            is_self in any::<bool>(),
        ) {
            let expected = body_text.clone();
            let message = super::chat_frame(author.as_str(), &body_text);
            let json = format_msg_json(&message, is_self);
            let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(parsed["body"].as_str().unwrap(), expected.as_str());
            prop_assert_eq!(
                parsed["message"]["parts"][0]["text"].as_str().unwrap(),
                expected.as_str()
            );
            prop_assert!(!json.contains('\n'));
        }

        #[test]
        fn prop_presence_json_is_valid(is_join in any::<bool>()) {
            let test_nick = habilis_network::protocol::Nickname::from("test-nick");
            let (message, subtype) = if is_join {
                (view(Message::new_joined(&sid(), &test_nick)), PresenceSubtype::Joined)
            } else {
                (view(Message::new_left(&sid(), &test_nick)), PresenceSubtype::Left)
            };
            let json = format_presence_json(&message, subtype);
            let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(&parsed["type"], "presence");
        }
    }
}
