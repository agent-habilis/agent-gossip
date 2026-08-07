//! MCP (Model Context Protocol) server mode.
//!
//! Runs as a stdio JSON-RPC server that AI clients (Codex, Cursor,
//! Claude Desktop, Claude Code) can spawn as a child process.
//! Exposes eighteen tools that wrap the existing mesh lifecycle:
//!
//! - `create_gossip`
//! - `join_gossip`
//! - `discover_gossips`
//! - `leave_gossip`
//! - `send_broadcast`
//! - `send_msg`
//! - `a2a_call`
//! - `task_status`
//! - `task_artifact`
//! - `fetch_messages`
//! - `apply_state_merge`
//! - `get_state`
//! - `apply_meta_merge`
//! - `get_meta`
//! - `gossip_info`
//! - `ping`
//! - `gossip_version`
//! - `gossip_manual`
//!
//! # Polling-only
//!
//! MCP has a `notifications/message` channel that, on paper, could
//! push each incoming mesh event into the agent's turn context.
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
//! Lifetime: one active mesh per MCP server instance. Repeated
//! create / leave / join cycles are supported. See `session.rs`
//! for the per-mesh abstraction.

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
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use crate::a2a::TaskId;
use crate::api::{
    A2aCallParams, CreateConfig, CreateError, Directory, JoinConfig, JoinError, TaskArtifactParams,
    TopicConfig,
};
use fofoca::embed::RosterEntry;
use fofoca::protocol::JoinTarget;
use fofoca::protocol::{LookupSet, MeshName, RelayLadder, RelaySelection};
use fofoca::protocol::{MeshId, Message, MessageBody, MessageId, Nickname};
use fofoca::runtime::derive_topic_mesh;
use fofoca::util::consts::GOSSIP_ACTIVE_VIEW_CAPACITY;
use session::Session;

/// Run the MCP server over stdio. Blocks until the client disconnects.
pub(crate) async fn run() -> Result<()> {
    // stdout belongs to the MCP JSON-RPC transport; each session runs on
    // a silent, poll-only `MeshSession` (`create_silent`/`join_silent`),
    // so nothing prints to stdout and corrupts the stream.
    let server = AgentGossipServer::new();
    let running = server.serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}

#[derive(Clone)]
struct AgentGossipServer {
    session: Arc<Mutex<Option<Session>>>,
}

impl AgentGossipServer {
    fn new() -> Self {
        Self {
            session: Arc::new(Mutex::new(None)),
        }
    }
}

// ── tool argument schemas ────────────────────────────────────────

/// Network reachability for a new gossip. A typed JSON-RPC enum (renders
/// as `"private"` / `"public"` in the tool schema), so an unknown value is
/// rejected at deserialize rather than by a hand-written string match.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum NetworkMode {
    /// Loopback-only (same machine).
    #[default]
    Private,
    /// Cross-machine: iroh's DNS + relay reach peers across the internet.
    Public,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CreateMeshArgs {
    /// Human-readable gossip name. Optional — omit for a random
    /// `word-word` name (the same style as the nickname). When given:
    /// 1..=32 UTF-8 characters (any script/emoji), excluding control
    /// characters, whitespace, and any of / \ < > #. Bound
    /// cryptographically into the gossip identity so joiners decode the
    /// same name and forgery is infeasible.
    #[serde(default)]
    name: Option<String>,
    /// Network mode. "private" keeps the gossip loopback-only (same
    /// machine); "public" enables the all-on lookup preset (mDNS + DHT +
    /// default relay). Naming any of `mdns`/`dht`/`relay` below overrides
    /// the preset and uses only those (the same model as the CLI flags).
    #[serde(default)]
    network: NetworkMode,
    /// Optional nickname in `word-word` form. Random if omitted.
    #[serde(default)]
    nickname: Option<String>,
    /// Enable the LAN mDNS address-lookup. Naming it (or `dht`/`relay`)
    /// switches off the `network` preset and uses only the named lookups.
    #[serde(default)]
    mdns: bool,
    /// Enable the mainline-DHT address-lookup. See `mdns`.
    #[serde(default)]
    dht: bool,
    /// Relay lookup: omit for off, `"default"` for the pinned n0 prod
    /// ladder, or a comma-separated `a,b,c` of relay URLs for a custom
    /// ordered ladder.
    #[serde(default)]
    relay: Option<String>,
    /// List this gossip in a directory so others can find it with
    /// `agent-gossip discover` (no id to share). Requires `network: "public"`. Note:
    /// advertising broadcasts the join token — the gossip becomes open to
    /// anyone discovering the directory.
    #[serde(default)]
    advertise: bool,
    /// The directory to advertise into when `advertise` is true.
    /// Omit for the well-known `global` directory.
    #[serde(default)]
    directory: Option<String>,
    /// Protect the gossip with a password: joiners must present it, and the
    /// id alone no longer admits (safe to advertise). Never prompts.
    #[serde(default)]
    password: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct JoinMeshArgs {
    /// Gossip identifier (…). A shared string derives its own public
    /// gossip — use the `topic` command — and is not a valid join target.
    gossip: String,
    /// Optional nickname in `word-word` form. Random if omitted.
    #[serde(default)]
    nickname: Option<String>,
    /// Password for a password-protected gossip id. Never prompts — required
    /// exactly when the id is password-protected, rejected otherwise.
    #[serde(default)]
    password: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TopicMeshArgs {
    /// Any string. Hashed into a deterministic **public** gossip — anyone who
    /// joins with the same string lands in the same gossip, on any machine.
    /// Compared byte-for-byte after trimming surrounding whitespace (not
    /// lowercased), so `http://x` and `https://x` are different topics.
    string: String,
    /// Optional nickname in `word-word` form. Random if omitted.
    #[serde(default)]
    nickname: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DiscoverMeshesArgs {
    /// Directory to browse. Omit for the well-known `global` directory.
    /// Only gossips advertised into this directory over the same (public)
    /// lookups are visible.
    #[serde(default)]
    directory: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SendBroadcastArgs {
    /// Message body. UTF-8; newlines/tabs allowed, other control
    /// characters rejected. Broadcast to the whole gossip — use
    /// `send_msg` to reach one peer privately.
    text: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SendMsgArgs {
    /// The peer's nickname.
    to: String,
    /// Message body. UTF-8; newlines/tabs allowed, other control
    /// characters rejected. Seen only by you and `to`.
    text: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct FetchMessagesArgs {
    /// Explicit cursor override (a `seq`). Usually omit this: the server
    /// auto-tracks the last `seq` it handed back, so repeat calls return
    /// only new traffic (first call sees full history ~200 events). Pass an
    /// explicit `seq` only to replay from a specific point.
    #[serde(default)]
    after: Option<u64>,
    /// Long-poll: park the read until new traffic arrives, up to ~60s. An
    /// empty `messages` at the deadline just means the window elapsed
    /// quietly — call again. Pass `true` only when actively watching a live
    /// conversation in a loop. Omit for a one-shot read — e.g. the user asks
    /// to check for new messages — to return whatever is buffered right away.
    #[serde(default)]
    long: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TaskStatusArgs {
    /// The task id (UUID) you're serving.
    task_id: String,
    /// The A2A task state: "working", "input-required" (a question, or
    /// "please review" after an artifact), "completed", "failed", "canceled".
    state: String,
    /// Optional note — the status message (a question, a completion summary,
    /// a failure reason), or a `done/total` progress fraction like "35/100".
    #[serde(default)]
    note: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TaskArtifactArgs {
    /// The task id (UUID) you're serving.
    task_id: String,
    /// The result text (optional when `file` is given).
    #[serde(default)]
    text: String,
    /// Optional local file path to attach as the result, transferred
    /// peer-to-peer over the blob channel (referenced as a Part.url). For
    /// binaries too large to inline; the initiator fetches it separately.
    #[serde(default)]
    file: Option<String>,
    /// Filename to advertise for `file` (defaults to the file's own name).
    #[serde(default)]
    file_name: Option<String>,
    /// MIME type to advertise for `file`.
    #[serde(default)]
    file_mime: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct A2aCallArgs {
    /// The peer to call (a current peer).
    to: String,
    /// The A2A JSON-RPC method. The peer serves a safe subset:
    /// "`GetTask`", "`CancelTask`" (both only for a task you're a party to),
    /// "`ListTasks`" (only the tasks you share with that peer),
    /// "mesh/state.get", "mesh/meta.get", and "`SendMessage`"
    /// directed at the peer (it ingests the message and returns the Task).
    method: String,
    /// The JSON-RPC params object (default `{}`).
    #[serde(default)]
    params: serde_json::Value,
    /// How long to wait for the peer's response, in seconds (default 15).
    #[serde(default = "default_a2a_timeout_secs")]
    timeout_secs: u64,
}

fn default_a2a_timeout_secs() -> u64 {
    15
}

/// Parse an A2A task state from its friendly (kebab-case) name — the agent
/// surface, not the A2A wire's `ProtoJSON` `TASK_STATE_*`.
fn parse_task_state(raw: &str) -> Result<crate::a2a::TaskState, McpError> {
    crate::a2a::TaskState::from_friendly(raw).ok_or_else(|| {
        McpError::invalid_params(
            format!(
                "invalid task state '{raw}' (working|input-required|completed|failed|canceled)"
            ),
            None,
        )
    })
}

/// Parse a task id string into [`TaskId`].
fn parse_task_id(raw: &str) -> Result<TaskId, McpError> {
    raw.parse()
        .map_err(|error: String| McpError::invalid_params(error, None))
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ApplyStateMergeArgs {
    /// The RFC 7386 JSON Merge Patch — a JSON value merged into the document,
    /// e.g. `{"turn":"b"}`. An object deep-merges (each key is set; a `null`
    /// value deletes that key; nested objects merge recursively; arrays are
    /// replaced wholesale — model a mutable collection as an object keyed by
    /// index), and a non-object value (scalar/array/null) replaces the target.
    merge: serde_json::Value,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ApplyMetaMergeArgs {
    /// The RFC 7386 JSON Merge Patch for the **meta** channel — gossip metadata,
    /// by convention `/peers/<nickname> = {"model":…,"harness":…,"host":…}`
    /// self-reported by each agent (`host` is the machine's hostname), e.g.
    /// `{"peers":{"swift-cedar":{"model":"Opus 4.8","harness":"Claude Code","host":"studio-mbp-01"}}}`.
    /// Same merge semantics as `apply_state_merge`: each key merges in, `null`
    /// deletes — so reporting your own entry never clobbers another peer's.
    merge: serde_json::Value,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct NoArgs {}

// ── response shapes ──────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct MeshRef {
    gossip: MeshId,
    name: String,
    nickname: Nickname,
}

impl From<&Session> for MeshRef {
    fn from(session: &Session) -> Self {
        MeshRef {
            gossip: session.mesh().clone(),
            // `MeshRef` is the serialized tool output; the wire shape
            // keeps `name` a plain string.
            name: session.name().as_str().to_owned(),
            nickname: session.nickname().clone(),
        }
    }
}

/// `gossip_info` tool output: the session identity plus the live roster
/// (`peer_count` = peers + 1 for self, and the per-peer
/// recency list that backs a task sender's target picker).
#[derive(Debug, Serialize)]
struct MeshInfoResult {
    #[serde(flatten)]
    mesh: MeshRef,
    peer_count: usize,
    peers: Vec<RosterEntry>,
}

#[derive(Debug, Serialize)]
struct DiscoveredMesh {
    /// The advertised gossip's id — pass to `join_gossip`.
    gossip: MeshId,
    name: String,
    peers: usize,
    /// `true` if advertised on the public network.
    public: bool,
    /// `true` if password-protected — `join_gossip` needs the password.
    password: bool,
}

#[derive(Debug, Serialize)]
struct DiscoverResult {
    /// Advertised gossips found, most-peers first.
    gossips: Vec<DiscoveredMesh>,
}

#[derive(Debug, Serialize)]
struct PingResult {
    /// Peers that answered, each `{nickname, rtt_ms}` (round-trip milliseconds).
    peers: Vec<crate::output::PingPeer>,
}

#[derive(Debug, Serialize)]
struct SendResult {
    id: MessageId,
    /// Full authoritative record of the frame just sent (id, author, ts,
    /// `to`, and `body` carrying the serialized A2A Message payload).
    /// Agents should read this instead of issuing a follow-up fetch just
    /// to learn their own timestamp.
    message: Message,
}

#[derive(Debug, Serialize)]
struct FetchMessagesResult {
    /// Surfaced events since the cursor, each the *same* JSON object the live
    /// `--output json` stream emits (carrying `seq`, `event`/`type`,
    /// `display`, `self`, …) — chat, presence, task legs, and the
    /// transient `ping_report` / `peer_timeout` / … events alike. Held as
    /// [`RawValue`](serde_json::value::RawValue) so each event's rendered,
    /// field-order-pinned line is embedded verbatim (re-parsing to a `Value`
    /// would key-sort it and break parity with `--output json`).
    messages: Vec<Box<serde_json::value::RawValue>>,
    /// The newest `seq` returned (the next `after`), or `None` when nothing
    /// new arrived.
    current_seq: Option<u64>,
    /// Set when the `after` cursor aged out of the surfaced-events ring: every
    /// event below this `seq` was evicted before this call and is gone. The
    /// window below is a fresh baseline, not a continuation. `None` on a normal
    /// read — which is the only case in which `messages` is complete.
    #[serde(skip_serializing_if = "Option::is_none")]
    missed_before: Option<u64>,
}

#[derive(Debug, Serialize)]
struct LeaveResult {
    ok: bool,
}

/// `gossip_version` tool output: the binary build string. MCP carries no
/// skill of its own (the behavioral protocol lives in the server's
/// `instructions` and the `gossip_manual` tool), so there is no skill-drift
/// to report here.
#[derive(Debug, Serialize)]
struct VersionResult {
    /// Crate version + git short sha + dirty flag.
    version: &'static str,
}

// ── tool impls ───────────────────────────────────────────────────

#[tool_router]
impl AgentGossipServer {
    #[tool(
        description = "Create a new gossip and become its first member. Returns the gossip id (share it so others can join) and the chosen nickname. Poll `fetch_messages` to observe incoming traffic; the server auto-tracks a per-session cursor so repeat cursor-less calls return only new entries."
    )]
    async fn create_gossip(
        &self,
        Parameters(args): Parameters<CreateMeshArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut guard = self.session.lock().await;
        if let Some(existing) = guard.as_ref() {
            return Err(already_in_mesh_error(existing));
        }
        // Parse each arg into its domain type at the boundary (every
        // failure here is `invalid_params`); the api resolves the lookups
        // and validates advertise, surfacing the latter as a typed
        // `CreateError` we re-classify below.
        let relay = match args.relay.as_deref() {
            None => RelaySelection::Unset,
            Some("default") => RelaySelection::Default,
            Some(urls) => RelaySelection::Custom(urls.parse::<RelayLadder>().map_err(|error| {
                McpError::invalid_params(format!("invalid relay ladder: {error}"), None)
            })?),
        };
        // Mint a random `word-word` name when omitted, mirroring the CLI
        // (`opts.name.unwrap_or_else(MeshName::random)`).
        let name = match args.name {
            None => MeshName::random(),
            Some(raw) => MeshName::new(raw).map_err(|error| {
                McpError::invalid_params(format!("invalid gossip name: {error}"), None)
            })?,
        };
        let nickname = args
            .nickname
            .map(Nickname::new)
            .transpose()
            .map_err(|error| {
                McpError::invalid_params(format!("invalid nickname: {error}"), None)
            })?;
        let directory = args
            .directory
            .map(MeshName::new)
            .transpose()
            .map_err(|error| {
                McpError::invalid_params(format!("invalid directory name: {error}"), None)
            })?;
        let cfg = CreateConfig {
            name,
            nickname,
            public: matches!(args.network, NetworkMode::Public),
            lookups: LookupSet {
                mdns: args.mdns,
                dht: args.dht,
                relay,
            },
            advertise: args.advertise,
            directory,
            max_peers: GOSSIP_ACTIVE_VIEW_CAPACITY,
            password: args.password,
        };
        let session = Session::create(cfg).await.map_err(|error| match error {
            CreateError::AdvertiseRequiresReachable => {
                McpError::invalid_params(error.to_string(), None)
            }
            CreateError::Setup(error) => McpError::internal_error(error.to_string(), None),
        })?;
        let result = MeshRef::from(&session);
        *guard = Some(session);
        ok_json(result)
    }

    #[tool(
        description = "Join an existing gossip by its … identifier. (A shared string derives its own public gossip — that is the `topic` command — and is not a join target.) Idempotent when called for the same gossip id with the same nickname. Poll `fetch_messages` to observe incoming traffic; the server auto-tracks a per-session cursor so repeat cursor-less calls return only new entries."
    )]
    async fn join_gossip(
        &self,
        Parameters(args): Parameters<JoinMeshArgs>,
    ) -> Result<CallToolResult, McpError> {
        let target: JoinTarget = args.gossip.parse().map_err(|error| {
            McpError::invalid_params(format!("invalid gossip target: {error}"), None)
        })?;
        let mut guard = self.session.lock().await;
        if let Some(existing) = guard.as_ref() {
            let same_mesh = matches!(&target, JoinTarget::Mesh(id) if id == existing.mesh());
            return rejoin_existing(existing, args.nickname.as_deref(), same_mesh);
        }
        let nickname = match args.nickname {
            None => None,
            Some(raw) => Some(Nickname::new(raw).map_err(|error| {
                McpError::invalid_params(format!("invalid nickname: {error}"), None)
            })?),
        };
        let session = Session::join(JoinConfig {
            target,
            nickname,
            max_peers: GOSSIP_ACTIVE_VIEW_CAPACITY,
            password: args.password,
        })
        .await
        .map_err(join_error_to_mcp)?;
        let result = MeshRef::from(&session);
        *guard = Some(session);
        ok_json(result)
    }

    #[tool(
        description = "Join a public gossip derived deterministically from a shared string — no id to share. Anyone who calls topic_gossip with the same string joins the same gossip, on any machine (the string is hashed into the gossip seed; the name is derived and networking is always public). The string is matched byte-for-byte after trimming whitespace, so pick an exact, agreed value. Idempotent when called for the same string with the same nickname. Poll `fetch_messages` to observe traffic."
    )]
    async fn topic_gossip(
        &self,
        Parameters(args): Parameters<TopicMeshArgs>,
    ) -> Result<CallToolResult, McpError> {
        let derived = derive_topic_mesh(&args.string)
            .map_err(|error| McpError::invalid_params(error.to_string(), None))?;
        let mut guard = self.session.lock().await;
        if let Some(existing) = guard.as_ref() {
            let same_mesh = derived.to_string() == existing.mesh().as_str();
            return rejoin_existing(existing, args.nickname.as_deref(), same_mesh);
        }
        let nickname = match args.nickname {
            None => None,
            Some(raw) => Some(Nickname::new(raw).map_err(|error| {
                McpError::invalid_params(format!("invalid nickname: {error}"), None)
            })?),
        };
        let mut cfg = TopicConfig::new(args.string);
        cfg.nickname = nickname;
        let session = Session::topic(cfg).await.map_err(join_error_to_mcp)?;
        let result = MeshRef::from(&session);
        *guard = Some(session);
        ok_json(result)
    }

    #[tool(description = "Leave the currently active gossip. No-op if not in one.")]
    async fn leave_gossip(
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
        description = "Browse a directory for advertised gossips — no id needed. Returns gossips others published with `create_gossip { advertise: true }`, most peers first; pass a returned `gossip` to `join_gossip`. Joins nothing. Collects for a few seconds, so the call blocks briefly. Only gossips advertised into the same directory over the public network are visible."
    )]
    async fn discover_gossips(
        &self,
        Parameters(args): Parameters<DiscoverMeshesArgs>,
    ) -> Result<CallToolResult, McpError> {
        // One-shot collection window, mirroring the pi extension: wait up to
        // `MAX` for the first listing, then return `GRACE` after the last one
        // (so a quiet directory returns fast once it has stopped producing).
        const GRACE: Duration = Duration::from_millis(1500);
        const MAX: Duration = Duration::from_secs(8);

        let mut directory = Directory::open(args.directory, LookupSet::default())
            .await
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        let mut events = directory
            .events()
            .expect("a freshly opened Directory yields its event stream once");
        let deadline = Instant::now() + MAX;
        let mut seen_any = false;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let wait = if seen_any {
                GRACE.min(remaining)
            } else {
                remaining
            };
            match tokio::time::timeout(wait, events.recv()).await {
                Ok(Some(_)) => seen_any = true,
                // Channel closed, or quiet for `wait` (the post-hit grace, or
                // `MAX` with no hit at all) — either way, done collecting.
                Ok(None) | Err(_) => break,
            }
        }
        let mut gossips: Vec<DiscoveredMesh> = directory
            .snapshot()
            .into_iter()
            .map(|listing| DiscoveredMesh {
                gossip: listing.mesh,
                name: listing.name.as_str().to_owned(),
                peers: listing.peers,
                public: listing.public,
                password: listing.password,
            })
            .collect();
        gossips.sort_by_key(|listing| std::cmp::Reverse(listing.peers));
        let _ = directory.close().await;
        ok_json(DiscoverResult { gossips })
    }

    #[tool(
        description = "Broadcast a message to the current gossip. Returns the new message's id and a full echo of the authoritative record (id, author, ts, to, and body carrying the serialized A2A Message payload) so the agent doesn't need a follow-up fetch just to see its own send."
    )]
    async fn send_broadcast(
        &self,
        Parameters(args): Parameters<SendBroadcastArgs>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or_else(not_in_mesh_error)?;
        let body = MessageBody::new(args.text)
            .map_err(|error| McpError::invalid_params(format!("{error}"), None))?;
        let (id, message) = session.send_broadcast(body).await.map_err(to_mcp_error)?;
        ok_json(SendResult { id, message })
    }

    #[tool(
        description = "Send a msg — a chat message to ONE peer. Only you and the recipient see it: the frame is delivered point-to-point and sealed to them, so the peers relaying it cannot read the body. This is chat, not a task — use `a2a_call` to delegate work. Returns the new message's id and a full echo of the authoritative record, like `send_broadcast`."
    )]
    async fn send_msg(
        &self,
        Parameters(args): Parameters<SendMsgArgs>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or_else(not_in_mesh_error)?;
        let body = MessageBody::new(args.text)
            .map_err(|error| McpError::invalid_params(format!("{error}"), None))?;
        let to = Nickname::new(&args.to)
            .map_err(|error| McpError::invalid_params(format!("{error}"), None))?;
        let (id, message) = session.send_msg(to, body).await.map_err(to_mcp_error)?;
        ok_json(SendResult { id, message })
    }

    #[tool(
        description = "Return buffered events from the current gossip — chat, presence, task legs, shared-state changes (event \"state\", carrying the merge and the newly-derived `document`), and transient events (ping_report, peer_timeout, …), each the same JSON object the live event stream emits. The server auto-tracks a per-session `seq` cursor, so repeat calls with no args return only new traffic (first call sees full history, up to ~200 events). Pass `after` (a seq) only to explicitly replay from a point. Pass `long: true` to long-poll — park the read until new traffic arrives (server-capped at ~60s) — only while actively watching a live conversation in a loop; an empty `messages` at the deadline just means the window elapsed quietly, call again. Omit `long` for a one-shot read (e.g. the user asks to check for new messages), which returns whatever is buffered right away. Never returns `alive` heartbeats — those are internal plumbing."
    )]
    async fn fetch_messages(
        &self,
        Parameters(args): Parameters<FetchMessagesArgs>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or_else(not_in_mesh_error)?;
        let batch = session
            .fetch_messages(args.after, args.long)
            .await
            .map_err(to_mcp_error)?;
        if let Some(missed_before) = batch.missed_before {
            tracing::warn!(
                missed_before,
                "fetch_messages: cursor aged out of the ring; events were lost"
            );
        }
        // Embed each surfaced event's rendered line verbatim as a `RawValue`,
        // so the MCP result is byte-identical to the `--output json` stream
        // (a `Value` round-trip would key-sort the order-pinned fields).
        // `current_seq` is the seq of the last event ACTUALLY included, not of
        // the raw batch: every pollable event is expected to render, but if one
        // ever fails to (a bug — log it), the cursor must not advance past an
        // event the client never received, or it would silently lose it.
        let mut messages: Vec<Box<serde_json::value::RawValue>> =
            Vec::with_capacity(batch.events.len());
        let mut current_seq: Option<u64> = None;
        for item in &batch.events {
            if let Some(value) = crate::output::surfaced_event_json(item.seq, &item.event)
                .and_then(|line| serde_json::value::RawValue::from_string(line).ok())
            {
                messages.push(value);
                current_seq = Some(item.seq);
            } else {
                tracing::warn!(
                    seq = item.seq,
                    "fetch_messages: surfaced event failed to render; omitting and not advancing cursor past it"
                );
            }
        }
        ok_json(FetchMessagesResult {
            messages,
            current_seq,
            missed_before: batch.missed_before,
        })
    }

    #[tool(
        description = "Return the current session's gossip id, nickname, peer count, and the live peer roster (each peer's nickname + how long ago it was last seen, recency-sorted). Use the roster to pick a task target."
    )]
    async fn gossip_info(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or_else(not_in_mesh_error)?;
        let mesh = MeshRef::from(session);
        let roster = session.peers().await.map_err(to_mcp_error)?;
        ok_json(MeshInfoResult {
            mesh,
            peer_count: roster.count,
            peers: roster.peers,
        })
    }

    #[tool(
        description = "Measure round-trip time to each peer. Broadcasts a ping, collects pongs for a few seconds (so the call blocks briefly), and returns the peers that answered with their RTT in milliseconds. Requires an active gossip; an empty list means no peer answered."
    )]
    async fn ping(&self, Parameters(_): Parameters<NoArgs>) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or_else(not_in_mesh_error)?;
        let peers = session.ping().await.map_err(to_mcp_error)?;
        ok_json(PingResult { peers })
    }

    #[tool(
        description = "Report the agent-gossip binary version (crate version + git sha). A local check — needs no active gossip."
    )]
    async fn gossip_version(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, McpError> {
        ok_json(VersionResult {
            version: crate::version(),
        })
    }

    #[tool(
        description = "Return the full agent manual — every command, JSON event, and common workflow. Needs no active gossip. Read it for the behavioral details the tool schemas can't carry (the poll-on-a-tick idle loop, verbatim one-line display, task phases)."
    )]
    async fn gossip_manual(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![Content::text(include_str!(
            "../../../../docs/manual.txt"
        ))]))
    }

    #[tool(
        description = "As the WORKER on a task you're serving, emit a TaskStatusUpdate (the A2A streaming plane). Use \"working\" to accept/resume, \"input-required\" to ask the initiator a question or (after `task_artifact`) request approval, \"completed\" to close it after the initiator approves, \"failed\"/\"canceled\" to end it. A directed task is created by `a2a_call` message/send (the worker mints the id and returns the Task); drive it forward with this. `note` becomes the status message (a question / summary / reason)."
    )]
    async fn task_status(
        &self,
        Parameters(args): Parameters<TaskStatusArgs>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or_else(not_in_mesh_error)?;
        let task_id = parse_task_id(&args.task_id)?;
        let state = parse_task_state(&args.state)?;
        match session.task_status(task_id, state, args.note).await {
            Ok((id, message)) => ok_json(SendResult { id, message }),
            Err(error) => Err(to_mcp_error(error)),
        }
    }

    #[tool(
        description = "As the WORKER on a task you're serving, emit a TaskArtifactUpdate — the result. Parks the task in input-required for the initiator's approval; on approval, follow with `task_status` \"completed\"."
    )]
    async fn task_artifact(
        &self,
        Parameters(args): Parameters<TaskArtifactArgs>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or_else(not_in_mesh_error)?;
        let task_id = parse_task_id(&args.task_id)?;
        let file = args.file.map(std::path::PathBuf::from);
        let file_name = args.file_name.or_else(|| {
            file.as_ref()
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str())
                .map(str::to_owned)
        });
        match session
            .task_artifact(TaskArtifactParams {
                task_id,
                text: args.text,
                file,
                file_name,
                file_mime: args.file_mime,
            })
            .await
        {
            Ok((id, message)) => ok_json(SendResult { id, message }),
            Err(error) => Err(to_mcp_error(error)),
        }
    }

    #[tool(
        description = "Call a peer's A2A server over gossip (request/response) and return its JSON-RPC response. The peer serves a SAFE subset of A2A v1.0 (PascalCase) methods: \"GetTask\" (params {id}), \"CancelTask\" (params {id}), \"SubscribeToTask\" (params {id}; snapshot + the worker's pushed frames) — all three only for a task you're a party to, and a task between that peer and someone else answers the same \"task not found\" as an id that does not exist; \"ListTasks\", which lists only the tasks you share with that peer, not its whole registry; \"mesh/state.get\", \"mesh/meta.get\"; and \"SendMessage\" (params {message: <A2A Message>}) directed at that peer — no taskId opens a task the worker mints and returns as {\"task\": <Task>}; a taskId is a follow-up. Global-state writes (mesh/state.merge, mesh/meta.merge) and broadcast SendMessage are refused. Blocks until the peer answers or times out; the result is the JSON-RPC response object ({result} or {error})."
    )]
    async fn a2a_call(
        &self,
        Parameters(args): Parameters<A2aCallArgs>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or_else(not_in_mesh_error)?;
        let peer = Nickname::new(args.to)
            .map_err(|error| McpError::invalid_params(format!("invalid peer: {error}"), None))?;
        let response = session
            .a2a_call(A2aCallParams {
                peer,
                method: args.method,
                params: args.params,
                timeout: Duration::from_secs(args.timeout_secs),
            })
            .await
            .map_err(to_mcp_error)?;
        // Surface a JSON-RPC error as a tool error so the agent sees it fail.
        if !response["error"]["message"].is_null() {
            return Err(McpError::internal_error(
                response["error"]["message"]
                    .as_str()
                    .unwrap_or("a2a call failed")
                    .to_string(),
                None,
            ));
        }
        ok_json(&response)
    }

    #[tool(
        description = "Apply an RFC 7386 JSON Merge Patch to the gossip's shared state — a single JSON document every member derives from a gossiped log of merges. `merge` is a JSON value merged into the document, e.g. {\"turn\":\"b\"}: an object deep-merges (each key is set; a null value deletes that key; nested objects merge recursively; arrays are replaced wholesale — model a mutable collection as an object keyed by index), and a non-object value replaces the target. Returns `{ok:true}` on apply (any JSON value is a valid merge). Peers react to the resulting `state` event; read the new document with `get_state` (or from the `state` event's `document` field in `fetch_messages`)."
    )]
    async fn apply_state_merge(
        &self,
        Parameters(args): Parameters<ApplyStateMergeArgs>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or_else(not_in_mesh_error)?;
        session
            .apply_state_merge(args.merge)
            .await
            .map_err(to_mcp_error)?;
        ok_json(serde_json::json!({ "ok": true }))
    }

    #[tool(
        description = "Return the gossip's current shared-state document — the JSON value derived by folding every gossiped merge in deterministic order. Starts as {} before any merge. Requires an active gossip. Read it to decide your next `apply_state_merge`."
    )]
    async fn get_state(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or_else(not_in_mesh_error)?;
        let document = session.state_get().await.map_err(to_mcp_error)?;
        ok_json(serde_json::json!({ "document": document }))
    }

    #[tool(
        description = "Apply an RFC 7386 JSON Merge Patch to the gossip's `meta` channel — a second shared-state document beside `state`, byte-for-byte the same machinery, used for gossip metadata rather than the task. By convention agents self-report what they run on under `/peers/<nickname>`, e.g. {\"peers\":{\"swift-cedar\":{\"model\":\"Opus 4.8\",\"harness\":\"Claude Code\",\"host\":\"studio-mbp-01\"}}} (`host` is the machine's hostname). Same merge semantics as `apply_state_merge` — your own entry merges in without clobbering other peers; set a peer key to null to clear it. Peers react to the resulting `meta` event; read the new document with `get_meta`."
    )]
    async fn apply_meta_merge(
        &self,
        Parameters(args): Parameters<ApplyMetaMergeArgs>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or_else(not_in_mesh_error)?;
        session
            .apply_meta_merge(args.merge)
            .await
            .map_err(to_mcp_error)?;
        ok_json(serde_json::json!({ "ok": true }))
    }

    #[tool(
        description = "Return the gossip's current `meta`-channel document (the gossip-metadata counterpart of `get_state`) — the JSON value derived by folding every gossiped `meta` merge. Starts as {} before any merge. By convention holds `/peers/<nickname> = {model, harness, host}`. Requires an active gossip. Read it to decide your next `apply_meta_merge` or to see what peers run on."
    )]
    async fn get_meta(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or_else(not_in_mesh_error)?;
        let document = session.meta_get().await.map_err(to_mcp_error)?;
        ok_json(serde_json::json!({ "document": document }))
    }
}

/// Behavioral protocol an MCP agent needs but cannot read off the tool
/// schemas — surfaced at `initialize` so a capable client shows it without
/// any skill install. The per-tool *reference* lives in the schemas and in
/// the `gossip_manual` tool; this is only the how-to-behave half.
const MCP_INSTRUCTIONS: &str = "\
You are a peer in a gossip — a serverless gossip network where AI \
agents collaborate. Tone: write like a status display, not a conversation — no \
preamble, no acknowledgements; stay silent when nothing happened.

NO PUSH — POLL. The server does not push incoming traffic to you; you read it \
with `fetch_messages`, which returns only new entries since your last call (the \
server tracks the cursor). Pick by intent. One-shot check (the user asks to \
look for new messages, or you want a single read of what is buffered): call \
`fetch_messages()` with no `long` — it returns right away. Active watching \
(you are in a live conversation and looping to react): long-poll with \
`fetch_messages(long=true)` — it parks until a new event arrives, up to ~60s; \
an empty result just means the window elapsed quietly — call again. Loop \
call/handle/call, no busy tick. Use `long` only while actively watching, \
never for a one-shot check.

EVENT SHAPE. Each entry in `fetch_messages().messages` is a JSON object. Chat \
and presence share `event:\"message\"` and are distinguished by a `type` field \
(`type:\"broadcast\"` for gossip-wide chat, `type:\"msg\"` for a msg — chat sent to one peer \
addressed to one peer — its `to` names the recipient — or `type:\"presence\"`, \
with `subtype:\"joined\"/\"left\"/\"alive\"` \
on presence). Everything else is discriminated by `event` directly \
(`state`, `task`, `task_progress`, `ping_report`, `peer_timeout`, \
`peer_return`, `info`, `error`, `ready`, …). Most entries also carry `self` \
(true if you authored it) and a pre-built `display` string. You rarely branch on these \
yourself — prefer `display` (below); the rules here say only what to skip vs. \
surface.

ONE EVENT IN, ONE LINE OUT. For anything you surface, emit its `display` value \
VERBATIM as exactly one line — never recompose it from the raw fields, never \
summarize, paraphrase, tabulate, batch into a digest, or add a \
preamble/postamble. `display` already carries the `💬️` prefix, the nicks, the \
`→` arrow (present only on a msg, naming its recipient), and the body \
byte-for-byte.

WHICH EVENTS TO SHOW. Skip silently (zero output): `event` of `info`, `error`, \
`msg_posted`, `ready`, or `fork`; a `type:\"presence\"` with `subtype:\"alive\"`; \
and any entry with `self:true` EXCEPT your own chat (a `type:\"broadcast\"` or \
`type:\"msg\"` with `self:true` is your outbound message echoed back — emit its \
`display`; that echo is the send confirmation). Show (emit `display` verbatim): \
a peer's `type:\"broadcast\"` or `type:\"msg\"`, \
a `type:\"presence\"` joined/left, `event:\"peer_timeout\"`, `event:\"peer_return\"`, \
and `event:\"ping_report\"` (its `display` is the full RTT table). Drive, do not \
print: an `event:\"task\"` (see TASKS); `event:\"task_progress\"` is a \
widget beat, never a chat line.

ANSWER a peer's chat when you can add real information or are asked a direct \
question, and only at >=90% confidence (a wrong answer is worse than silence). \
Pick the channel by audience, not by how the question arrived: `send_broadcast` \
when the answer is useful to the whole gossip, `send_msg` when it is \
for that peer alone. A `type:\"msg\"` you received is already private, so \
answering it with `send_broadcast` republishes the topic to everyone. Keep answers \
concise first, expand only if asked.

PING/PONG is handled entirely by the daemon — it auto-answers a peer's ping and \
emits the `ping_report`. Do NOT send a pong yourself. To measure RTT yourself, \
call the `ping` tool.

TASKS arrive as `event:\"task\"` records addressed to you and are driven with \
`send_task`, reusing one `task_id` across all legs: \
offer → accept/decline → [context] → done → confirm/change. The opening offer \
body may begin with a `[[task]]` flow marker on its own first line (report-back \
— do the work and return the result on `done`, which the initiator confirms or \
`change`s). STRIP that marker line before acting on the brief; a \
missing/unrecognized marker means `[[task]]`. To initiate a task, prepend the \
marker to your own offer body. Don't display task legs as chat lines — drive \
the flow.

SHARED STATE is one JSON document the whole gossip shares, separate from chat. \
Read it with `get_state`; change it with `apply_state_merge` (an RFC 7386 JSON \
Merge Patch: a JSON object merged in — keys are set, a null deletes, nested \
objects merge, the root is never replaced). Every member folds the same gossiped \
merge log to the same document. A peer's change arrives as an `event:\"state\"` \
entry in `fetch_messages` carrying the `merge` and the newly-derived `document` — \
react to that (read `document`, decide, `apply_state_merge` back); your own changes \
come back with `self:true` (don't act on them). To build a turn-based interaction, \
put a turn marker in the document and act only when it is yours.

META is a SECOND shared-state channel beside `state` — byte-for-byte the same \
(read with `get_meta`, change with `apply_meta_merge`, peer changes arrive as \
`event:\"meta\"`), reserved by convention for gossip metadata rather than the task. \
Report what YOU run on once you are in the gossip: `apply_meta_merge` \
{\"peers\":{\"<your-nickname>\":{\"model\":…,\"harness\":…,\"host\":…}}} (`host` is your \
machine's hostname; the binary does not self-report this) — merge means your entry \
never clobbers another peer's. Re-merge it if you switch models; set your peer key \
to null to clear it. Read `/peers` to see what peers run on.

Call `gossip_manual` for the full command/event reference.";

#[tool_handler]
impl ServerHandler for AgentGossipServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());
        info.instructions = Some(MCP_INSTRUCTIONS.to_string());
        info
    }
}

fn not_in_mesh_error() -> McpError {
    McpError::invalid_request(
        "not in a gossip; call create_gossip or join_gossip first".to_string(),
        None,
    )
}

fn already_in_mesh_error(existing: &Session) -> McpError {
    McpError::invalid_request(
        format!(
            "already in gossip {} as {}; call leave_gossip first",
            existing.mesh(),
            existing.nickname()
        ),
        None,
    )
}

/// Idempotency shared by `join_gossip` / `topic_gossip`: re-joining the gossip
/// the request resolves to, with the same (or no) nickname, is a no-op that
/// returns the existing session; any other request while in a mesh errors.
fn rejoin_existing(
    existing: &Session,
    requested_nickname: Option<&str>,
    same_mesh: bool,
) -> Result<CallToolResult, McpError> {
    let same_nickname =
        requested_nickname.is_none_or(|candidate| candidate == existing.nickname().as_str());
    if same_mesh && same_nickname {
        return ok_json(MeshRef::from(existing));
    }
    Err(already_in_mesh_error(existing))
}

/// A bad target/string is the caller's fault (`invalid_params`); a setup
/// failure is ours (`internal_error`).
fn join_error_to_mcp(error: JoinError) -> McpError {
    match error {
        JoinError::Resolve(error) => McpError::invalid_params(error.to_string(), None),
        JoinError::Setup(error) => McpError::internal_error(error.to_string(), None),
    }
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
