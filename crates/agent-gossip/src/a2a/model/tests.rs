use serde_json::json;

use super::{
    Artifact, Message, MessageId, Part, Role, Task, TaskArtifactUpdate, TaskId, TaskState,
    TaskStatus, TaskStatusUpdate,
};

fn fixture_message() -> Message {
    Message {
        message_id: MessageId::from("00000000-0000-0000-0000-000000000001"),
        role: Role::User,
        parts: vec![Part::text("What is Rust?")],
        context_id: Some("test".to_string()),
        task_id: None,
        reference_task_ids: Vec::new(),
        extensions: vec![crate::a2a::EXT_MESH_BROADCAST.to_string()],
        metadata: None,
    }
}

#[test]
fn message_id_rejects_non_uuid() {
    let not_a_uuid = json!("not-a-uuid");
    assert!(
        serde_json::from_value::<MessageId>(not_a_uuid.clone()).is_err(),
        "correlation relies on ids being unique, which free-form strings do not guarantee"
    );
    assert!(
        serde_json::from_value::<TaskId>(not_a_uuid).is_err(),
        "a forged task id from a peer must fail at the parse, before it keys any record"
    );
}

#[test]
fn part_requires_exactly_one_content_field() {
    let text_and_url = json!({"text": "hi", "url": "x"});
    let filename_only = json!({"filename": "a.txt"});
    let text_only = json!({"text": "hi"});
    let url_with_media_type = json!({"url": "https://x", "mediaType": "text/plain"});
    let raw_only = json!({"raw": "aGk="});
    assert!(
        serde_json::from_value::<Part>(text_and_url).is_err(),
        "a part with text and url is rejected"
    );
    assert!(
        serde_json::from_value::<Part>(filename_only).is_err(),
        "a part with only a filename is rejected"
    );
    assert!(
        serde_json::from_value::<Part>(text_only).is_ok(),
        "a text part is accepted"
    );
    assert!(
        serde_json::from_value::<Part>(url_with_media_type).is_ok(),
        "a url part with a media type is accepted"
    );
    assert!(
        serde_json::from_value::<Part>(raw_only).is_ok(),
        "a raw part is accepted"
    );
}

#[test]
fn task_state_is_screaming_snake_on_the_wire() {
    assert_eq!(
        serde_json::to_value(TaskState::InputRequired).expect("a test value serializes to JSON"),
        json!("TASK_STATE_INPUT_REQUIRED"),
        "the A2A v1.0 spec fixes this spelling, and other implementations match on it"
    );
    assert_eq!(
        TaskState::InputRequired.friendly_name(),
        "input-required",
        "the friendly name keeps its kebab case"
    );
    assert_eq!(
        serde_json::to_value(Role::User).expect("a test value serializes to JSON"),
        json!("ROLE_USER"),
        "the A2A v1.0 spec fixes this spelling, so a renamed variant would break every peer"
    );
}

#[test]
fn terminal_states_are_the_four_closed_ones() {
    for state in [
        TaskState::Completed,
        TaskState::Canceled,
        TaskState::Failed,
        TaskState::Rejected,
    ] {
        assert!(
            state.is_terminal(),
            "no leg may reopen a task after one of these four, so the sweep can reap it"
        );
    }
    for state in [
        TaskState::Submitted,
        TaskState::Working,
        TaskState::InputRequired,
        TaskState::AuthRequired,
        TaskState::Unspecified,
    ] {
        assert!(!state.is_terminal(), "an open state is not terminal");
    }
}

#[test]
fn message_round_trips() {
    let message = fixture_message();
    let json = serde_json::to_string(&message).expect("a test value serializes to JSON");
    let parsed: Message =
        serde_json::from_str(&json).expect("the JSON text comes from the test itself");
    assert_eq!(parsed, message, "a message survives a json round trip");
}

mod snapshots {
    use habilis_network::protocol::messages::Verified;

    use super::{
        Artifact, Part, Task, TaskArtifactUpdate, TaskId, TaskState, TaskStatus, TaskStatusUpdate,
        fixture_message,
    };
    use crate::a2a::wire;

    #[test]
    fn snap_a2a_message() {
        insta::assert_snapshot!(
            serde_json::to_string(&fixture_message()).expect("a test value serializes to JSON")
        );
    }

    /// The full **wire bytes** of a gossip frame carrying an A2A JSON-RPC
    /// request: `a2a_req` tag, directed `to`, correlated `corr`, body = the
    /// `{method, params}` envelope.
    ///
    /// This pair lives here rather than in the engine because it is an *A2A*
    /// contract. The engine pins the frame envelope with model-neutral bodies
    /// — it must not know this shape — so the concatenation (real A2A payload
    /// inside a real frame) is pinned by the layer that owns the payload.
    #[test]
    fn snap_a2a_req_frame_wire() {
        let message = Verified::fixture_unchecked(
            habilis_network::protocol::Message::fixture(
                habilis_network::protocol::MessageKind::app_to(
                    wire::REQ,
                    habilis_network::protocol::Nickname::from("addressed-nick"),
                    Some(wire::corr_id("00000000-0000-0000-0000-0000000000aa")),
                ),
                r#"{"method":"tasks/list","params":{}}"#,
            ),
            habilis_network::protocol::AuthorKey::from([0; 32]),
        );
        let bytes: Vec<u8> = message
            .serialize()
            .expect("the test message serializes")
            .as_bytes()
            .to_vec();
        insta::assert_snapshot!(String::from_utf8(bytes).expect("the bytes are printable ASCII"));
    }

    /// The response leg of [`snap_a2a_req_frame_wire`], echoing its `corr`.
    #[test]
    fn snap_a2a_resp_frame_wire() {
        let message = Verified::fixture_unchecked(
            habilis_network::protocol::Message::fixture(
                habilis_network::protocol::MessageKind::app_to(
                    wire::RESP,
                    habilis_network::protocol::Nickname::from("addressed-nick"),
                    Some(wire::corr_id("00000000-0000-0000-0000-0000000000aa")),
                ),
                r#"{"result":{"tasks":[]}}"#,
            ),
            habilis_network::protocol::AuthorKey::from([0; 32]),
        );
        let bytes: Vec<u8> = message
            .serialize()
            .expect("the test message serializes")
            .as_bytes()
            .to_vec();
        insta::assert_snapshot!(String::from_utf8(bytes).expect("the bytes are printable ASCII"));
    }

    #[test]
    fn snap_a2a_task() {
        let task = Task {
            id: TaskId::from("550e8400-e29b-41d4-a716-446655440000"),
            context_id: "test".to_string(),
            status: TaskStatus {
                state: TaskState::Working,
                message: None,
                timestamp: Some("2026-01-01T00:00:00Z".to_string()),
            },
            artifacts: Vec::new(),
            history: vec![fixture_message()],
            metadata: None,
        };
        insta::assert_snapshot!(
            serde_json::to_string(&task).expect("a test value serializes to JSON")
        );
    }

    #[test]
    fn snap_a2a_status_update() {
        let update = TaskStatusUpdate {
            task_id: TaskId::from("550e8400-e29b-41d4-a716-446655440000"),
            context_id: "test".to_string(),
            status: TaskStatus {
                state: TaskState::InputRequired,
                message: None,
                timestamp: None,
            },
            metadata: Some(serde_json::json!({"mesh:review": true})),
        };
        insta::assert_snapshot!(
            serde_json::to_string(&update).expect("a test value serializes to JSON")
        );
    }

    #[test]
    fn snap_a2a_artifact_update() {
        let update = TaskArtifactUpdate {
            task_id: TaskId::from("550e8400-e29b-41d4-a716-446655440000"),
            context_id: "test".to_string(),
            artifact: Artifact {
                artifact_id: "00000000-0000-0000-0000-00000000000a".to_string(),
                parts: vec![Part::text("the parser now handles multiline")],
                name: None,
                description: None,
                extensions: Vec::new(),
                metadata: None,
            },
            append: None,
            last_chunk: Some(true),
            metadata: None,
        };
        insta::assert_snapshot!(
            serde_json::to_string(&update).expect("a test value serializes to JSON")
        );
    }
}
