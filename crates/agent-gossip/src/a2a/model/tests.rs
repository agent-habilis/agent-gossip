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
    assert!(serde_json::from_str::<MessageId>("\"not-a-uuid\"").is_err());
    assert!(serde_json::from_str::<TaskId>("\"not-a-uuid\"").is_err());
}

#[test]
fn part_requires_exactly_one_content_field() {
    assert!(serde_json::from_str::<Part>(r#"{"text":"hi","url":"x"}"#).is_err());
    assert!(serde_json::from_str::<Part>(r#"{"filename":"a.txt"}"#).is_err());
    assert!(serde_json::from_str::<Part>(r#"{"text":"hi"}"#).is_ok());
    assert!(
        serde_json::from_str::<Part>(r#"{"url":"https://x","mediaType":"text/plain"}"#).is_ok()
    );
    assert!(serde_json::from_str::<Part>(r#"{"raw":"aGk="}"#).is_ok());
}

#[test]
fn task_state_is_screaming_snake_on_the_wire() {
    assert_eq!(
        serde_json::to_string(&TaskState::InputRequired).unwrap(),
        "\"TASK_STATE_INPUT_REQUIRED\""
    );
    assert_eq!(TaskState::InputRequired.as_str(), "input-required");
    assert_eq!(serde_json::to_string(&Role::User).unwrap(), "\"ROLE_USER\"");
}

#[test]
fn terminal_states_are_the_four_closed_ones() {
    for state in [
        TaskState::Completed,
        TaskState::Canceled,
        TaskState::Failed,
        TaskState::Rejected,
    ] {
        assert!(state.is_terminal());
    }
    for state in [
        TaskState::Submitted,
        TaskState::Working,
        TaskState::InputRequired,
        TaskState::AuthRequired,
        TaskState::Unspecified,
    ] {
        assert!(!state.is_terminal());
    }
}

#[test]
fn message_round_trips() {
    let message = fixture_message();
    let json = serde_json::to_string(&message).unwrap();
    let parsed: Message = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, message);
}

mod snapshots {
    use super::{
        Artifact, Part, Task, TaskArtifactUpdate, TaskId, TaskState, TaskStatus, TaskStatusUpdate,
        fixture_message,
    };

    #[test]
    fn snap_a2a_message() {
        insta::assert_snapshot!(serde_json::to_string(&fixture_message()).unwrap());
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
        let message = habilis_network::protocol::message::Verified::fixture_unchecked(
            habilis_network::protocol::Message::fixture(
                habilis_network::protocol::MessageKind::app_to(
                    crate::a2a::wire::REQ,
                    habilis_network::protocol::Nickname::from("addressed-nick"),
                    Some(habilis_network::protocol::CorrId::from(
                        "00000000-0000-0000-0000-0000000000aa",
                    )),
                ),
                r#"{"method":"tasks/list","params":{}}"#,
            ),
            habilis_network::protocol::AuthorKey::from_bytes([0; 32]),
        );
        insta::assert_snapshot!(
            String::from_utf8(message.serialize().unwrap().as_bytes().to_vec()).unwrap()
        );
    }

    /// The response leg of [`snap_a2a_req_frame_wire`], echoing its `corr`.
    #[test]
    fn snap_a2a_resp_frame_wire() {
        let message = habilis_network::protocol::message::Verified::fixture_unchecked(
            habilis_network::protocol::Message::fixture(
                habilis_network::protocol::MessageKind::app_to(
                    crate::a2a::wire::RESP,
                    habilis_network::protocol::Nickname::from("addressed-nick"),
                    Some(habilis_network::protocol::CorrId::from(
                        "00000000-0000-0000-0000-0000000000aa",
                    )),
                ),
                r#"{"result":{"tasks":[]}}"#,
            ),
            habilis_network::protocol::AuthorKey::from_bytes([0; 32]),
        );
        insta::assert_snapshot!(
            String::from_utf8(message.serialize().unwrap().as_bytes().to_vec()).unwrap()
        );
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
        insta::assert_snapshot!(serde_json::to_string(&task).unwrap());
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
        insta::assert_snapshot!(serde_json::to_string(&update).unwrap());
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
        insta::assert_snapshot!(serde_json::to_string(&update).unwrap());
    }
}
