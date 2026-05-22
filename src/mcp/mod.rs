//! MCP (Model Context Protocol) server mode.
//!
//! Runs as a stdio JSON-RPC server that AI clients (Codex, Cursor,
//! Claude Desktop, Claude Code) can spawn as a child process.
//! Exposes six tools that wrap the existing swarm lifecycle:
//!
//! - `create_swarm`
//! - `join_swarm`
//! - `leave_swarm`
//! - `send_message`
//! - `fetch_messages`
//! - `swarm_info`
//!
//! # Polling-only
//!
//! MCP has a `notifications/message` channel that, on paper, could
//! push each incoming swarm event into the agent's turn context.
//! In practice (April 2026) no major MCP client — Cursor, Claude
//! Desktop, Codex — surfaces those notifications to the agent,
//! so agents must call `fetch_messages` explicitly to see new
//! traffic. The server auto-tracks a per-session cursor so
//! repeat cursor-less calls return only new traffic.
//!
//! The Claude Code skill (`skill/SKILL.md`) bypasses MCP and reads
//! the CLI's `--output json` stdout stream directly, which *is* a
//! live push — preserve that path.
//!
//! Lifetime: one active swarm per MCP server instance. Repeated
//! create / leave / join cycles are supported. See `session.rs`
//! for the per-swarm abstraction.

mod session;

use anyhow::Result;
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, Content, ErrorData as McpError, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::protocol::swarm::{SwarmMode, SwarmName, resolve_relay};
use crate::protocol::{MessageBody, MessageId, Nickname, SwarmId};
use session::Session;

/// Run the MCP server over stdio. Blocks until the client disconnects.
pub(crate) async fn run() -> Result<()> {
    // stdout belongs to the MCP JSON-RPC transport; the per-session
    // `Output::silent()` (see `spawn_session`) suppresses any print
    // that would corrupt the stream.
    let server = AgentSwarmServer::new();
    let running = server.serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}

#[derive(Clone)]
struct AgentSwarmServer {
    session: Arc<Mutex<Option<Session>>>,
}

impl AgentSwarmServer {
    fn new() -> Self {
        Self {
            session: Arc::new(Mutex::new(None)),
        }
    }
}

// ── tool argument schemas ────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CreateSwarmArgs {
    /// Human-readable swarm name. Required. 1..=32 UTF-8 characters
    /// (any script/emoji), excluding control characters, whitespace, and
    /// any of / \ < > #. Bound cryptographically into the swarm identity
    /// so joiners decode the same name and forgery is infeasible.
    name: String,
    /// Network mode. "private" keeps the swarm loopback-only (same
    /// machine). "public" uses iroh's DNS + N0 relay to reach peers
    /// across the internet.
    #[serde(default = "default_network")]
    network: String,
    /// Optional nickname in `word-word` form. Random if omitted.
    #[serde(default)]
    nickname: Option<String>,
    /// Custom relay URL. Requires `network: "public"`.
    #[serde(default)]
    relay: Option<String>,
}

fn default_network() -> String {
    "private".to_string()
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct JoinSwarmArgs {
    /// Swarm identifier (ahs…), a domain (example.com), or a git
    /// repo URL (github.com/user/repo, gitlab.com/user/repo,
    /// bitbucket.org/user/repo). Non-id values are resolved via
    /// `/.well-known/agent-habilis-swarm`.
    swarm: String,
    /// Optional nickname in `word-word` form. Random if omitted.
    #[serde(default)]
    nickname: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SendMessageArgs {
    /// Message body. UTF-8; newlines/tabs allowed, other control
    /// characters rejected.
    text: String,
    /// Optional target nickname to address this message to.
    #[serde(default)]
    reply: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct FetchMessagesArgs {
    /// Explicit cursor override. Usually omit this: the server
    /// auto-tracks the last message it handed back, so repeat calls
    /// return only new traffic (first call sees full history ~200
    /// messages). Pass an explicit id only to replay from a
    /// specific point.
    #[serde(default)]
    after: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct NoArgs {}

// ── response shapes ──────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct SwarmRef {
    swarm: SwarmId,
    name: String,
    nickname: Nickname,
}

impl From<&Session> for SwarmRef {
    fn from(session: &Session) -> Self {
        SwarmRef {
            swarm: session.swarm.clone(),
            name: session.name.clone(),
            nickname: session.nickname.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
struct SendMessageResult {
    id: MessageId,
    /// Full authoritative record of the message just sent (id,
    /// author, ts, body, reply) — same shape `fetch_messages`
    /// returns. Agents should read this instead of issuing a
    /// follow-up fetch just to learn their own timestamp.
    message: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct FetchMessagesResult {
    messages: Vec<serde_json::Value>,
    current_id: Option<MessageId>,
}

#[derive(Debug, Serialize)]
struct LeaveResult {
    ok: bool,
}

// ── tool impls ───────────────────────────────────────────────────

#[tool_router]
impl AgentSwarmServer {
    #[tool(
        description = "Create a new swarm and become its first member. Returns the swarm id (share it so others can join) and the chosen nickname. Poll `fetch_messages` to observe incoming traffic; the server auto-tracks a per-session cursor so repeat cursor-less calls return only new entries."
    )]
    async fn create_swarm(
        &self,
        Parameters(args): Parameters<CreateSwarmArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut guard = self.session.lock().await;
        if let Some(existing) = guard.as_ref() {
            return Err(already_in_swarm_error(existing));
        }
        let mode = SwarmMode::from_network_name(&args.network)
            .map_err(|error| McpError::invalid_params(error.to_string(), None))?;
        let relay = resolve_relay(mode, args.relay.as_deref())
            .map_err(|error| McpError::invalid_params(error.to_string(), None))?;
        let name = SwarmName::new(args.name).map_err(|error| {
            McpError::invalid_params(format!("invalid swarm name: {error}"), None)
        })?;
        let nickname = match args.nickname {
            None => Nickname::random(),
            Some(raw) => Nickname::new(raw).map_err(|error| {
                McpError::invalid_params(format!("invalid nickname: {error}"), None)
            })?,
        };
        let session = Session::create(mode, name, relay, nickname)
            .await
            .map_err(to_mcp_error)?;
        let result = SwarmRef::from(&session);
        *guard = Some(session);
        ok_json(result)
    }

    #[tool(
        description = "Join an existing swarm. Accepts an ahs… identifier, a domain (resolves /.well-known/agent-habilis-swarm), or a git repo URL. Idempotent when called for the same swarm id with the same nickname. Poll `fetch_messages` to observe incoming traffic; the server auto-tracks a per-session cursor so repeat cursor-less calls return only new entries."
    )]
    async fn join_swarm(
        &self,
        Parameters(args): Parameters<JoinSwarmArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut guard = self.session.lock().await;
        if let Some(existing) = guard.as_ref() {
            // Idempotent: re-joining the same swarm with either the
            // same nickname or no nickname is a no-op, not an error.
            let same_nickname = args
                .nickname
                .as_deref()
                .is_none_or(|candidate| candidate == existing.nickname.as_str());
            if existing.swarm.as_str() == args.swarm && same_nickname {
                return ok_json(SwarmRef::from(existing));
            }
            return Err(already_in_swarm_error(existing));
        }
        let nickname = match args.nickname {
            None => Nickname::random(),
            Some(raw) => Nickname::new(raw).map_err(|error| {
                McpError::invalid_params(format!("invalid nickname: {error}"), None)
            })?,
        };
        let session = Session::join(&args.swarm, nickname)
            .await
            .map_err(to_mcp_error)?;
        let result = SwarmRef::from(&session);
        *guard = Some(session);
        ok_json(result)
    }

    #[tool(description = "Leave the currently active swarm. No-op if not in one.")]
    async fn leave_swarm(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut guard = self.session.lock().await;
        let session = guard.take();
        drop(guard);
        if let Some(active) = session {
            active.leave().await;
        }
        ok_json(LeaveResult { ok: true })
    }

    #[tool(
        description = "Broadcast a message to the current swarm. Returns the new message's id and a full echo of the authoritative record (id, author, ts, body, reply) — same shape `fetch_messages` returns — so the agent doesn't need a follow-up fetch just to see its own send."
    )]
    async fn send_message(
        &self,
        Parameters(args): Parameters<SendMessageArgs>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or_else(not_in_swarm_error)?;
        let reply = match args.reply {
            None => None,
            Some(raw) => Some(Nickname::new(raw).map_err(|error| {
                McpError::invalid_params(format!("invalid reply target: {error}"), None)
            })?),
        };
        let body = MessageBody::new(args.text)
            .map_err(|error| McpError::invalid_params(format!("{error}"), None))?;
        let (id, message) = session
            .send_message(body, reply)
            .await
            .map_err(to_mcp_error)?;
        ok_json(SendMessageResult { id, message })
    }

    #[tool(
        description = "Return buffered messages from the current swarm. The server auto-tracks a per-session cursor, so repeat calls with no args return only new traffic (first call sees full history, up to ~200 messages). Pass `after` only to explicitly replay from a specific id. Never returns `alive` heartbeats — those are internal plumbing."
    )]
    async fn fetch_messages(
        &self,
        Parameters(args): Parameters<FetchMessagesArgs>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or_else(not_in_swarm_error)?;
        let after = match args.after {
            None => None,
            Some(raw) => Some(MessageId::new(raw).map_err(|error| {
                McpError::invalid_params(format!("invalid after cursor: {error}"), None)
            })?),
        };
        let (messages, current_id) = session.fetch_messages(after).await.map_err(to_mcp_error)?;
        ok_json(FetchMessagesResult {
            messages,
            current_id,
        })
    }

    #[tool(description = "Return the current session's swarm id, nickname, and participant count.")]
    async fn swarm_info(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or_else(not_in_swarm_error)?;
        ok_json(SwarmRef::from(session))
    }
}

#[tool_handler]
impl ServerHandler for AgentSwarmServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }
}

fn not_in_swarm_error() -> McpError {
    McpError::invalid_request(
        "not in a swarm; call create_swarm or join_swarm first".to_string(),
        None,
    )
}

fn already_in_swarm_error(existing: &Session) -> McpError {
    McpError::invalid_request(
        format!(
            "already in swarm {} as {}; call leave_swarm first",
            existing.swarm, existing.nickname
        ),
        None,
    )
}

fn to_mcp_error<E: std::fmt::Display>(error: E) -> McpError {
    McpError::internal_error(error.to_string(), None)
}

/// One-content JSON success result.
fn ok_json<T: Serialize>(value: T) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![
        Content::json(value).map_err(to_mcp_error)?,
    ]))
}
