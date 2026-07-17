//! Output-module tests: the JSON wire format (shape assertions + insta
//! snapshots + proptests). The wire serializers under test live in
//! [`super::json`].

use super::json::{SimpleEvent, format_msg_json, format_presence_json};
use agent_habilis_mesh::protocol::{Message, MessageKind, PresenceSubtype};

fn parse(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|error| panic!("invalid JSON: {error}\n{text}"))
}

// ── format_msg_json: msg type ──────────────────────────────

fn nick(name: &str) -> agent_habilis_mesh::protocol::Nickname {
    agent_habilis_mesh::protocol::Nickname::from(name)
}

fn sid() -> agent_habilis_mesh::protocol::MeshId {
    agent_habilis_mesh::protocol::MeshId::from("💬://test")
}

/// A chat frame carrying a real A2A payload, its frame id pinned to the
/// payload's `messageId` — the invariant `broadcast_message` establishes.
fn chat_frame(author: &str, text: &str) -> Message {
    let payload = crate::a2a::gossip::chat_message(&sid(), text);
    let frame_id = agent_habilis_mesh::protocol::MessageId::new(payload.message_id.as_str())
        .expect("an a2a id is a valid frame id");
    let body = crate::a2a::gossip::payload_body(&payload).expect("payload serializes");
    Message::new_app(
        &sid(),
        &nick(author),
        agent_habilis_mesh::protocol::AppFrameParams {
            tag: agent_habilis_mesh::protocol::AppTag::from(crate::a2a::wire::MSG),
            to: None,
            corr: None,
            body,
        },
    )
    .with_id(frame_id)
}

#[test]
fn json_message_has_all_fields() {
    let msg = chat_frame("alice", "hello");
    let json = format_msg_json(&msg, false);
    let parsed = parse(&json);
    assert_eq!(parsed["event"], "message");
    assert_eq!(parsed["type"], "msg");
    assert_eq!(parsed["room"], sid().as_str());
    assert_eq!(parsed["author"], "alice");
    assert_eq!(parsed["body"], "hello");
    assert!(parsed["to"].is_null());
    assert_eq!(parsed["self"], false);
    assert!(parsed["id"].is_string());
    assert!(parsed["ts"].is_number());
    // The embedded A2A object rides along whole (v1.0 ProtoJSON: no inline
    // `kind`, `ROLE_USER`), its id the frame's id.
    assert_eq!(parsed["message"]["messageId"], parsed["id"]);
    assert_eq!(parsed["message"]["role"], "ROLE_USER");
    assert_eq!(parsed["message"]["parts"][0]["text"], "hello");
    // Unsigned (test fixture): the pubkey field is omitted.
    assert!(parsed.get("pubkey").is_none());
}

#[test]
fn json_signed_message_exposes_full_pubkey() {
    // A real (signed) message carries the author's full Ed25519 public key
    // (hex) in the JSON event — the identity behind the display nickname.
    let identity = agent_habilis_mesh::protocol::identity::Identity::generate();
    let msg = chat_frame("alice", "hi").signed(&identity);
    let parsed = parse(&format_msg_json(&msg, false));
    let pubkey = parsed["pubkey"].as_str().expect("signed event has pubkey");
    assert_eq!(pubkey, msg.pubkey, "event pubkey must be the message's key");
    assert_eq!(pubkey.len(), 64, "full 32-byte key as hex");
}

#[test]
fn ready_event_drift_is_present_only_when_stale() {
    use super::OutputEvent;
    let make = |drift: Option<&str>| {
        super::json::event_json(&OutputEvent::Ready {
            mesh: sid(),
            name: agent_habilis_mesh::protocol::mesh::MeshName::new("team").unwrap(),
            nickname: nick("alice"),
            drift: drift.map(str::to_owned),
            a2a_port: None,
        })
        .expect("ready event produces a JSON line")
    };

    // In sync: no `drift` key on the wire.
    let clean = parse(&make(None));
    assert_eq!(clean["event"], "ready");
    assert!(clean.get("drift").is_none());

    // Stale: the warning rides along verbatim.
    let stale = parse(&make(Some(
        "⚠️ room skill out of date. Run `agent-gossip plug` to update",
    )));
    assert_eq!(
        stale["drift"],
        "⚠️ room skill out of date. Run `agent-gossip plug` to update"
    );
}

#[test]
fn fork_event_json_shape() {
    use super::OutputEvent;
    let pubkey = "deadbeef".repeat(8); // 64 hex
    let json = super::json::event_json(&OutputEvent::Fork {
        nickname: nick("alice"),
        pubkey: pubkey.clone(),
        seq: 7,
    })
    .expect("fork event produces a JSON line");
    let parsed = parse(&json);
    assert_eq!(parsed["event"], "fork");
    assert_eq!(parsed["nickname"], "alice");
    assert_eq!(parsed["pubkey"], pubkey);
    assert_eq!(parsed["seq"], 7);
}

#[test]
fn json_self_flag_true() {
    let msg = chat_frame("alice", "hi");
    let parsed = parse(&format_msg_json(&msg, true));
    assert_eq!(parsed["self"], true);
}

#[test]
fn json_body_escapes_quotes() {
    let msg = chat_frame("alice", r#"say "hello""#);
    let json = format_msg_json(&msg, false);
    let parsed = parse(&json);
    assert_eq!(parsed["body"], r#"say "hello""#);
}

#[test]
fn json_body_escapes_backslashes() {
    let msg = chat_frame("alice", r"path\to\file");
    let json = format_msg_json(&msg, false);
    let parsed = parse(&json);
    assert_eq!(parsed["body"], r"path\to\file");
}

// ── format_msg_json: presence type ─────────────────────────

#[test]
fn json_presence_joined() {
    let msg = Message::new_joined(&sid(), &nick("alice"));
    let parsed = parse(&format_presence_json(&msg, PresenceSubtype::Joined));
    assert_eq!(parsed["event"], "message");
    assert_eq!(parsed["type"], "presence");
    assert_eq!(parsed["subtype"], "joined");
    assert_eq!(parsed["author"], "alice");
    assert_eq!(parsed["room"], sid().as_str());
    assert_eq!(parsed["display"], "💬️ `<alice>` has joined");
}

#[test]
fn json_presence_left() {
    let msg = Message::new_left(&sid(), &nick("bob"));
    let parsed = parse(&format_presence_json(&msg, PresenceSubtype::Left));
    assert_eq!(parsed["subtype"], "left");
    assert_eq!(parsed["author"], "bob");
}

// ── task type ──────────────────────────────────────────────

/// The value cluster for the `status_frame` test helper: the crafted
/// frame's task id (with optional beat metadata).
struct StatusFrameArgs<'a> {
    task_id: &'a str,
    state: crate::a2a::TaskState,
    note: Option<&'a str>,
    metadata: Option<serde_json::Value>,
}

/// A task **status** frame carrying `state` (with optional beat metadata).
fn status_frame(author: &str, to: &str, args: StatusFrameArgs<'_>) -> Message {
    let StatusFrameArgs {
        task_id,
        state,
        note,
        metadata,
    } = args;
    let tid = crate::a2a::TaskId::from_uuid_str(task_id).expect("valid task id");
    let update = crate::a2a::gossip::status_update(
        &sid(),
        crate::a2a::gossip::StatusUpdateParams {
            task_id: &tid,
            state,
            note,
            metadata,
        },
    );
    let body = crate::a2a::gossip::payload_body(&update).expect("payload serializes");
    let kind = MessageKind::app_to(crate::a2a::wire::STATUS, nick(to), None);
    Message::new_frame(&sid(), &nick(author), kind, body)
}

#[test]
fn task_event_json_shape() {
    use super::OutputEvent;
    let msg = status_frame(
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
    let json = super::json::event_json(&OutputEvent::Task {
        msg: Box::new(msg),
        is_self: false,
    })
    .expect("task event produces a JSON line");
    let parsed = parse(&json);
    assert_eq!(parsed["event"], "task");
    assert_eq!(parsed["author"], "drift-oak");
    assert_eq!(parsed["to"], "calm-otter");
    assert_eq!(parsed["task_id"], "550e8400-e29b-41d4-a716-446655440000");
    assert_eq!(parsed["kind"], "status-update");
    assert_eq!(parsed["state"], "working");
    assert_eq!(parsed["self"], false);
    assert!(parsed["id"].is_string());
    assert!(parsed["ts"].is_number());
    // The A2A construct rides along whole for A2A-aware consumers (v1.0
    // ProtoJSON: no inline `kind`, `TASK_STATE_*`).
    assert_eq!(parsed["payload"]["status"]["state"], "TASK_STATE_WORKING");
    assert!(parsed.get("phase").is_none());
    // Distinct top-level event — never the `message` family, no `type` key.
    assert!(parsed.get("type").is_none());
}

#[test]
fn task_self_echo_flag() {
    use super::OutputEvent;
    let msg = status_frame(
        "drift-oak",
        "calm-otter",
        StatusFrameArgs {
            task_id: "550e8400-e29b-41d4-a716-446655440000",
            state: crate::a2a::TaskState::Completed,
            note: Some("looks good"),
            metadata: None,
        },
    );
    let json = super::json::event_json(&OutputEvent::Task {
        msg: Box::new(msg),
        is_self: true,
    })
    .expect("task event produces a JSON line");
    let parsed = parse(&json);
    assert_eq!(parsed["self"], true);
    assert_eq!(parsed["kind"], "status-update");
    assert_eq!(parsed["state"], "completed");
    assert_eq!(parsed["payload"]["status"]["state"], "TASK_STATE_COMPLETED");
}

#[test]
fn task_progress_event_json_shape() {
    use super::OutputEvent;
    let msg = status_frame(
        "calm-otter",
        "drift-oak",
        StatusFrameArgs {
            task_id: "550e8400-e29b-41d4-a716-446655440000",
            state: crate::a2a::TaskState::Working,
            note: None,
            metadata: Some(crate::a2a::gossip::beat_metadata(Some((35, 100)))),
        },
    );
    let json = super::json::event_json(&OutputEvent::Task {
        msg: Box::new(msg),
        is_self: false,
    })
    .expect("progress event produces a JSON line");
    let parsed = parse(&json);
    // A beat renders as a distinct `task_progress` event with its
    // `done`/`total`, never as a content `task` event.
    assert_eq!(parsed["event"], "task_progress");
    assert_eq!(parsed["task_id"], "550e8400-e29b-41d4-a716-446655440000");
    assert_eq!(parsed["done"], 35);
    assert_eq!(parsed["total"], 100);
    assert!(parsed.get("phase").is_none());
    assert_eq!(
        parsed["display"],
        "💬️ task progress `<calm-otter>` → `<drift-oak>`: 35/100"
    );
}

#[test]
fn task_timeout_event_json_shape() {
    use super::OutputEvent;
    use crate::a2a::TaskId;
    let json = super::json::event_json(&OutputEvent::TaskTimeout {
        task_id: TaskId::from("550e8400-e29b-41d4-a716-446655440000"),
    })
    .expect("task_timeout event produces a JSON line");
    let parsed = parse(&json);
    assert_eq!(parsed["event"], "task_timeout");
    assert_eq!(parsed["task_id"], "550e8400-e29b-41d4-a716-446655440000");
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
    assert_eq!(parsed["event"], "peer_timeout");
    assert_eq!(parsed["nickname"], "ball-blue");
    assert_eq!(parsed["last_seen_secs_ago"], 94);
    assert_eq!(parsed["display"], "💬️ `<ball-blue>` went quiet");
}

#[test]
fn peer_return_json_shape() {
    let json = serde_json::to_string(&SimpleEvent::PeerReturn {
        nickname: "ball-blue",
        display: "💬️ `<ball-blue>` came back".to_owned(),
    })
    .unwrap();
    let parsed = parse(&json);
    assert_eq!(parsed["event"], "peer_return");
    assert_eq!(parsed["nickname"], "ball-blue");
    assert_eq!(parsed["display"], "💬️ `<ball-blue>` came back");
}

// ── all outputs are valid single-line JSON ─────────────────────

#[test]
fn json_output_is_single_line() {
    let msgs: Vec<String> = vec![
        format_msg_json(&chat_frame("alice", "hello world"), false),
        format_presence_json(
            &Message::new_joined(&sid(), &nick("charlie")),
            PresenceSubtype::Joined,
        ),
        format_presence_json(
            &Message::new_left(&sid(), &nick("dave")),
            PresenceSubtype::Left,
        ),
    ];
    for json in msgs {
        assert!(!json.contains('\n'), "JSON should be single line: {json}");
        assert!(parse(&json).is_object());
    }
}

mod snapshots {
    use super::{format_msg_json, format_presence_json};
    use agent_habilis_mesh::protocol::{Message, MessageKind, PresenceSubtype};

    /// Deterministic task frames for the wire-pinned snapshots: ids fixed,
    /// timestamps from the fixture.
    fn snap_frame(kind: MessageKind, body: &str) -> Message {
        Message::fixture(kind, body)
    }

    const SNAP_TASK_ID: &str = "550e8400-e29b-41d4-a716-446655440000";

    fn snap_status(
        state: crate::a2a::TaskState,
        note: Option<&str>,
        metadata: Option<serde_json::Value>,
    ) -> String {
        let tid = crate::a2a::TaskId::from(SNAP_TASK_ID);
        let mut update = crate::a2a::gossip::status_update(
            &agent_habilis_mesh::protocol::MeshId::from("💬://test"),
            crate::a2a::gossip::StatusUpdateParams {
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
        let body = crate::a2a::gossip::payload_body(&update).expect("payload serializes");
        let frame = snap_frame(
            MessageKind::app_to(
                crate::a2a::wire::STATUS,
                agent_habilis_mesh::protocol::Nickname::from("calm-otter"),
                None,
            ),
            body.as_str(),
        );
        super::super::json::format_task_json(&frame, false)
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
            Some(crate::a2a::gossip::beat_metadata(Some((35, 100)))),
        ));
    }

    #[test]
    fn snap_task_done() {
        let tid = crate::a2a::TaskId::from(SNAP_TASK_ID);
        let mut update = crate::a2a::gossip::artifact_update(
            &agent_habilis_mesh::protocol::MeshId::from("💬://test"),
            &tid,
            "2 findings: missing await in recv; unbounded buffer in flush",
        );
        update.artifact.artifact_id = "00000000-0000-0000-0000-00000000000a".to_string();
        let body = crate::a2a::gossip::payload_body(&update).expect("payload serializes");
        let frame = snap_frame(
            MessageKind::app_to(
                crate::a2a::wire::ARTIFACT,
                agent_habilis_mesh::protocol::Nickname::from("calm-otter"),
                None,
            ),
            body.as_str(),
        );
        insta::assert_snapshot!(super::super::json::format_task_json(&frame, false));
    }

    /// A deterministic chat frame for snapshots: the payload's random
    /// `messageId` is pinned to the fixture id so the snapshot is stable and
    /// the frame keeps the id == messageId invariant.
    fn snap_chat_frame(text: &str) -> Message {
        let mut payload = crate::a2a::gossip::chat_message(
            &agent_habilis_mesh::protocol::MeshId::from("💬://test"),
            text,
        );
        payload.message_id = crate::a2a::MessageId::from("00000000-0000-0000-0000-000000000001");
        let body = crate::a2a::gossip::payload_body(&payload).expect("payload serializes");
        Message::fixture(
            MessageKind::app_broadcast(crate::a2a::wire::MSG),
            body.as_str(),
        )
    }

    #[test]
    fn snap_msg_message() {
        insta::assert_snapshot!(format_msg_json(&snap_chat_frame("What is Rust?"), false));
    }

    #[test]
    fn snap_presence_joined() {
        let msg = Message::fixture(
            MessageKind::Presence {
                subtype: PresenceSubtype::Joined,
            },
            "",
        );
        insta::assert_snapshot!(format_presence_json(&msg, PresenceSubtype::Joined));
    }

    #[test]
    fn snap_presence_left() {
        let msg = Message::fixture(
            MessageKind::Presence {
                subtype: PresenceSubtype::Left,
            },
            "",
        );
        insta::assert_snapshot!(format_presence_json(&msg, PresenceSubtype::Left));
    }

    /// The `{"event":"state",…}` line a shared-state change emits on the
    /// `--output json` stream: header + the applied `merge` document + the
    /// newly-derived `document` + `display` + `self`. Field order is wire-pinned.
    fn snap_state(merge: &str, document: &serde_json::Value, is_self: bool) -> String {
        let body = format!(r#"{{"k":"merge","merge":{merge}}}"#);
        super::super::json::format_state_json(
            agent_habilis_mesh::protocol::Channel::State,
            &Message::fixture(MessageKind::State, &body),
            document,
            is_self,
        )
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
    use proptest::{
        collection::vec as arb_vec, prelude::any, prop_assert, prop_assert_eq, proptest,
        strategy::Strategy,
    };

    use super::{PresenceSubtype, format_msg_json, format_presence_json, sid};
    use agent_habilis_mesh::protocol::Message;

    fn arb_ascii_body() -> impl Strategy<Value = String> {
        arb_vec(0x20u8..0x7Eu8, 0..200).prop_map(|bytes| String::from_utf8(bytes).unwrap())
    }

    fn arb_nickname() -> impl Strategy<Value = agent_habilis_mesh::protocol::Nickname> {
        "[a-z]{3,8}-[a-z]{3,8}"
            .prop_map(|raw| agent_habilis_mesh::protocol::Nickname::new(raw).unwrap())
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
            let msg = super::chat_frame(author.as_str(), &body_text);
            let json = format_msg_json(&msg, is_self);
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
            let test_nick = agent_habilis_mesh::protocol::Nickname::from("test-nick");
            let (msg, subtype) = if is_join {
                (Message::new_joined(&sid(), &test_nick), PresenceSubtype::Joined)
            } else {
                (Message::new_left(&sid(), &test_nick), PresenceSubtype::Left)
            };
            let json = format_presence_json(&msg, subtype);
            let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(&parsed["type"], "presence");
        }
    }
}
