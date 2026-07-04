pub(crate) mod card;
pub mod gossip;
pub(crate) mod gossip_rpc;
pub(crate) mod http;
pub(crate) mod rpc;
pub(crate) mod task;

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

/// The A2A spec revision these objects conform to; carried in every
/// [`AgentCard`]'s interfaces so clients can gate on it. v1.0 encodes every
/// object as `ProtoJSON` (`SCREAMING_SNAKE` enums, no inline `kind` tags).
pub const PROTOCOL_VERSION: &str = "1.0";

/// Custom `protocolBinding` URI for the always-on gossip binding — the swarm's
/// serverless A2A transport, declared in every card's `supportedInterfaces`.
pub const GOSSIP_BINDING: &str = "https://agent-habilis.dev/a2a/binding/gossip/v1";

/// Extension URI declared on broadcast chat: A2A messaging is point-to-point,
/// so a swarm-wide message marks itself rather than pretending to be directed.
pub const EXT_SWARM_BROADCAST: &str = "https://agent-habilis.dev/a2a/ext/swarm-broadcast/v1";
/// Extension URI for the shared-state channels (`swarm/state.get|merge`).
pub const EXT_SWARM_STATE: &str = "https://agent-habilis.dev/a2a/ext/swarm-state/v1";
/// Extension URI advertising that a member serves A2A **over gossip**
/// (`A2aReq`/`A2aResp`, request/response and request/stream) for a safe method
/// subset — so a peer knows it can call this member without the localhost HTTP
/// binding. This is also how task delegation and streaming work: a directed
/// `SendMessage` creates a task the worker mints and returns; `SubscribeToTask`
/// opens a stream the worker propagates over gossip.
pub const EXT_SWARM_A2A_RPC: &str = "https://agent-habilis.dev/a2a/ext/swarm-a2a-rpc/v1";
/// Extension URI advertising the blob channel: a large file on a `Part` travels
/// as a `url` reference (a `📦…` ticket) and its bytes stream point-to-point over
/// a dedicated QUIC ALPN, SHA-256-verified — instead of inlining over gossip.
pub const EXT_SWARM_BLOB: &str = "https://agent-habilis.dev/a2a/ext/swarm-blob/v1";
/// Extension URI advertising end-to-end sealing: directed frames (those with a
/// `to`) are encrypted to the recipient's X25519 key, carried in this extension's
/// `params.x25519` (base58). Relays forward + verify the signature but cannot
/// read the body. Broadcast stays public.
pub const EXT_SWARM_SEAL: &str = "https://agent-habilis.dev/a2a/ext/swarm-seal/v1";

/// Metadata key marking a status update as a **liveness beat** (keepalive /
/// progress): plumbing, never logged, surfaced only as `task_progress`.
pub const META_BEAT: &str = "swarm:beat";
/// Metadata keys carrying a progress fraction on a beat.
pub const META_DONE: &str = "swarm:done";
/// See [`META_DONE`].
pub const META_TOTAL: &str = "swarm:total";
/// Metadata key naming why a terminal status was emitted by the daemon
/// itself (e.g. `"timeout"` on the idle-debounce cancel).
pub const META_REASON: &str = "swarm:reason";

/// A UUID-string id newtype with validating deserialize, so a crafted
/// non-UUID id is rejected at parse. One macro, three ids: the sender-minted
/// message id, the server-minted task id, and the gossip RPC correlation id.
macro_rules! uuid_id {
    ($name:ident, $error:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let raw = String::deserialize(deserializer)?;
                Uuid::parse_str(&raw).map_err(|_| serde::de::Error::custom($error))?;
                Ok(Self(raw))
            }
        }

        impl $name {
            #[must_use]
            pub fn random() -> Self {
                Self(Uuid::new_v4().to_string())
            }

            /// Adopt an existing UUID string (e.g. a frame id, which shares
            /// the UUID form). `None` for a non-UUID.
            #[must_use]
            pub fn from_uuid_str(raw: &str) -> Option<Self> {
                Uuid::parse_str(raw).ok().map(|_| Self(raw.to_string()))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl std::str::FromStr for $name {
            type Err = String;

            fn from_str(raw: &str) -> Result<Self, Self::Err> {
                Self::from_uuid_str(raw).ok_or_else(|| $error.to_string())
            }
        }

        #[cfg(test)]
        impl From<&str> for $name {
            fn from(raw: &str) -> Self {
                Uuid::parse_str(raw).expect($error);
                Self(raw.to_string())
            }
        }
    };
}

uuid_id!(MessageId, "invalid a2a message id");
uuid_id!(TaskId, "invalid a2a task id");
uuid_id!(A2aRpcId, "invalid a2a rpc id");

/// A2A v1.0 `Role`, `ProtoJSON` `SCREAMING_SNAKE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    #[serde(rename = "ROLE_UNSPECIFIED")]
    Unspecified,
    #[serde(rename = "ROLE_USER")]
    User,
    #[serde(rename = "ROLE_AGENT")]
    Agent,
}

/// A2A v1.0 `Part`: a proto `oneof content` — exactly one of `text` / `raw`
/// (base64 bytes) / `url` / `data` — plus the `filename` / `mediaType` /
/// `metadata` siblings. `ProtoJSON` puts the set oneof member at top level (no
/// `kind` discriminator), so this is a struct with a validating deserialize
/// that enforces exactly-one-of at parse.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl Part {
    /// A plain text part.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Part {
            text: Some(text.into()),
            ..Part::default()
        }
    }

    /// The display projection: the text content, or a bracketed placeholder for
    /// a file (`url`/`raw`) or structured (`data`) part. One projection, reused
    /// wherever parts are rendered to a line.
    #[must_use]
    pub fn display(&self) -> String {
        if let Some(text) = &self.text {
            text.clone()
        } else if self.url.is_some() || self.raw.is_some() {
            format!("[file {}]", self.filename.as_deref().unwrap_or("unnamed"))
        } else {
            "[data]".to_string()
        }
    }
}

impl<'de> Deserialize<'de> for Part {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire {
            text: Option<String>,
            raw: Option<String>,
            url: Option<String>,
            data: Option<serde_json::Value>,
            filename: Option<String>,
            media_type: Option<String>,
            metadata: Option<serde_json::Value>,
        }
        let raw = Wire::deserialize(deserializer)?;
        let content = [
            raw.text.is_some(),
            raw.raw.is_some(),
            raw.url.is_some(),
            raw.data.is_some(),
        ]
        .into_iter()
        .filter(|set| *set)
        .count();
        if content != 1 {
            return Err(serde::de::Error::custom(
                "a part must carry exactly one of text, raw, url, or data",
            ));
        }
        Ok(Part {
            text: raw.text,
            raw: raw.raw,
            url: raw.url,
            data: raw.data,
            filename: raw.filename,
            media_type: raw.media_type,
            metadata: raw.metadata,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub message_id: MessageId,
    pub role: Role,
    pub parts: Vec<Part>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reference_task_ids: Vec<TaskId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl Message {
    /// A minimal message with a fresh id and one text part; callers stamp
    /// `context_id` / `task_id` / `extensions` as the operation requires.
    #[must_use]
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Message {
            message_id: MessageId::random(),
            role,
            parts: vec![Part::text(text)],
            context_id: None,
            task_id: None,
            reference_task_ids: Vec::new(),
            extensions: Vec::new(),
            metadata: None,
        }
    }
}

/// A2A v1.0 `TaskState`, `ProtoJSON` `SCREAMING_SNAKE` with the `TASK_STATE_`
/// prefix on the wire; [`TaskState::as_str`] keeps the friendly kebab name for
/// our own `--output json` stream and CLI, which are the agent API, not A2A.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    #[serde(rename = "TASK_STATE_UNSPECIFIED")]
    Unspecified,
    #[serde(rename = "TASK_STATE_SUBMITTED")]
    Submitted,
    #[serde(rename = "TASK_STATE_WORKING")]
    Working,
    #[serde(rename = "TASK_STATE_INPUT_REQUIRED")]
    InputRequired,
    #[serde(rename = "TASK_STATE_COMPLETED")]
    Completed,
    #[serde(rename = "TASK_STATE_CANCELED")]
    Canceled,
    #[serde(rename = "TASK_STATE_FAILED")]
    Failed,
    #[serde(rename = "TASK_STATE_REJECTED")]
    Rejected,
    #[serde(rename = "TASK_STATE_AUTH_REQUIRED")]
    AuthRequired,
}

impl TaskState {
    /// Terminal states end a task for good: no leg may follow, and a frame
    /// that tries is dropped (resurrect-after-terminal is an attack shape).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskState::Completed | TaskState::Canceled | TaskState::Failed | TaskState::Rejected
        )
    }

    /// The friendly kebab name for display lines, our `--output json` stream,
    /// and the `ahsw a2a status --state` flag — the agent-facing surface, kept
    /// readable while the A2A wire carries the `ProtoJSON` `TASK_STATE_*` form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Unspecified => "unspecified",
            TaskState::Submitted => "submitted",
            TaskState::Working => "working",
            TaskState::InputRequired => "input-required",
            TaskState::Completed => "completed",
            TaskState::Canceled => "canceled",
            TaskState::Failed => "failed",
            TaskState::Rejected => "rejected",
            TaskState::AuthRequired => "auth-required",
        }
    }

    /// Parse the friendly kebab name (the CLI `--state` surface).
    #[must_use]
    pub fn from_friendly(raw: &str) -> Option<Self> {
        Some(match raw {
            "submitted" => TaskState::Submitted,
            "working" => TaskState::Working,
            "input-required" => TaskState::InputRequired,
            "completed" => TaskState::Completed,
            "canceled" => TaskState::Canceled,
            "failed" => TaskState::Failed,
            "rejected" => TaskState::Rejected,
            "auth-required" => TaskState::AuthRequired,
            _ => return None,
        })
    }
}

impl fmt::Display for TaskState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A serde `with` module that (de)serializes a [`TaskState`] by its **friendly**
/// kebab name — for our internal surfaces (the IPC socket, MCP tool args),
/// keeping the `ProtoJSON` `TASK_STATE_*` form confined to the A2A wire objects.
pub mod friendly_state {
    use super::TaskState;
    use serde::{Deserialize, Deserializer, Serializer};

    /// # Errors
    /// Never — serialization of a small enum cannot fail.
    pub fn serialize<S: Serializer>(state: &TaskState, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(state.as_str())
    }

    /// # Errors
    /// An unknown friendly state name.
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<TaskState, D::Error> {
        let raw = String::deserialize(deserializer)?;
        TaskState::from_friendly(&raw)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown task state: {raw}")))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatus {
    pub state: TaskState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Box<Message>>,
    /// RFC 3339 timestamp — the envelope's clock stays authoritative for
    /// ordering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub artifact_id: String,
    pub parts: Vec<Part>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: TaskId,
    pub context_id: String,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<Artifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatusUpdate {
    pub task_id: TaskId,
    pub context_id: String,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskArtifactUpdate {
    pub task_id: TaskId,
    pub context_id: String,
    pub artifact: Artifact,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub append: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_chunk: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// A2A v1.0 `SendMessageResponse`: a proto oneof — `ProtoJSON` serializes the set
/// member by name, so the JSON-RPC `result` is `{"task":…}` or `{"message":…}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SendMessageResponse {
    Task(Task),
    Message(Message),
}

/// A2A v1.0 `StreamResponse`: the SSE / gossip-stream payload oneof. Each
/// streamed event is one of these, ProtoJSON-tagged by member name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamResponse {
    Task(Task),
    Message(Message),
    StatusUpdate(TaskStatusUpdate),
    ArtifactUpdate(TaskArtifactUpdate),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentExtension {
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streaming: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_notifications: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<AgentExtension>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extended_agent_card: Option<bool>,
}

/// A2A v1.0 `AgentInterface`: one reachable endpoint for a binding. A member
/// always declares its gossip interface (the pubkey-addressed mesh endpoint);
/// a `--a2a-serve` daemon additionally declares its localhost JSON-RPC one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInterface {
    pub url: String,
    pub protocol_binding: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    pub protocol_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_modes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_modes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCard {
    pub name: String,
    pub description: String,
    pub supported_interfaces: Vec<AgentInterface>,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation_url: Option<String>,
    pub capabilities: AgentCapabilities,
    pub default_input_modes: Vec<String>,
    pub default_output_modes: Vec<String>,
    pub skills: Vec<AgentSkill>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security_schemes: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::{
        Artifact, Message, MessageId, Part, Role, Task, TaskArtifactUpdate, TaskId, TaskState,
        TaskStatus, TaskStatusUpdate,
    };

    fn fixture_message() -> Message {
        Message {
            message_id: MessageId::from("00000000-0000-0000-0000-000000000001"),
            role: Role::User,
            parts: vec![Part::text("What is Rust?")],
            context_id: Some("🐝test".to_string()),
            task_id: None,
            reference_task_ids: Vec::new(),
            extensions: vec![super::EXT_SWARM_BROADCAST.to_string()],
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
            Artifact, Part, Task, TaskArtifactUpdate, TaskId, TaskState, TaskStatus,
            TaskStatusUpdate, fixture_message,
        };

        #[test]
        fn snap_a2a_message() {
            insta::assert_snapshot!(serde_json::to_string(&fixture_message()).unwrap());
        }

        #[test]
        fn snap_a2a_task() {
            let task = Task {
                id: TaskId::from("550e8400-e29b-41d4-a716-446655440000"),
                context_id: "🐝test".to_string(),
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
                context_id: "🐝test".to_string(),
                status: TaskStatus {
                    state: TaskState::InputRequired,
                    message: None,
                    timestamp: None,
                },
                metadata: Some(serde_json::json!({"swarm:review": true})),
            };
            insta::assert_snapshot!(serde_json::to_string(&update).unwrap());
        }

        #[test]
        fn snap_a2a_artifact_update() {
            let update = TaskArtifactUpdate {
                task_id: TaskId::from("550e8400-e29b-41d4-a716-446655440000"),
                context_id: "🐝test".to_string(),
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
}

// ── A2A HTTP tunnel (origin: bridge an external A2A server over the swarm) ──
use std::time::Duration;

use iroh::Endpoint;

mod card_rewrite;
mod connect;
mod directory;
mod expose;
mod ticket;

#[cfg(test)]
mod harness_tests;

pub(crate) use connect::connect;
pub(crate) use directory::{TicketDirectory, TicketDirectoryEvent, TicketListing};
pub(crate) use expose::expose;

/// ALPN for the a2a bridge — a raw bidirectional QUIC stream with its own
/// protocol identity, distinct from the gossip protocol's `GOSSIP_ALPN`.
pub(crate) const A2A_ALPN: &[u8] = b"agent-habilis-swarm/a2a/1";

/// Length of the bearer-capability secret carried in an a2a ticket, and of the
/// auth token that opens every bi-stream (the raw secret, or its Argon2id
/// stretch when passworded — same size either way).
pub(crate) const SECRET_LEN: usize = 32;

/// Whether `ticket` decodes as a password-protected a2a ticket — the CLI's
/// prompt-before-connect check. `false` on any decode failure: the connect
/// path re-decodes and surfaces the real error.
pub(crate) fn ticket_requires_password(ticket: &str) -> bool {
    ticket::A2aTicket::decode(ticket).is_ok_and(|decoded| decoded.password)
}

/// Best-effort wait (≤5s) for the endpoint to publish reachable addresses, so a
/// freshly-printed ticket resolves immediately. Never blocks forever.
async fn wait_online(endpoint: &Endpoint) {
    let _ = tokio::time::timeout(Duration::from_secs(5), endpoint.online()).await;
}

/// Present the exposer's status and the consumer's ready-to-run command on
/// **stdout** — the exposer's product (its stdout carries no data; that flows
/// over the network), and stderr stays errors-only. Human (default) shows a
/// status line + the command in blue on a terminal (plain when piped); `json`
/// is the bare command for machines (no status/colors).
fn announce(json: bool, status: &str, command: &str) {
    tracing::info!("{status}");
    if json {
        println!("{command}");
        return;
    }
    let (open, close) = if crate::output::stdout_color() {
        (crate::output::style::BLUE, crate::output::style::RESET)
    } else {
        ("", "")
    };
    println!("📡 {status}");
    println!("other peer can connect with: {open}{command}{close}");
}
