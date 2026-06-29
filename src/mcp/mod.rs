//! MCP (Model Context Protocol) server mode.
//!
//! Runs as a stdio JSON-RPC server that AI clients (Codex, Cursor,
//! Claude Desktop, Claude Code) can spawn as a child process.
//! Exposes thirteen tools that wrap the existing swarm lifecycle:
//!
//! - `create_swarm`
//! - `join_swarm`
//! - `discover_swarms`
//! - `leave_swarm`
//! - `send_message`
//! - `send_exchange`
//! - `fetch_messages`
//! - `apply_state_patch`
//! - `get_state`
//! - `swarm_info`
//! - `ping`
//! - `swarm_version`
//! - `swarm_manual`
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
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use crate::daemon::state::RosterEntry;
use crate::embed::{CreateConfig, CreateError, Directory, JoinConfig};
use crate::gossip::StatePatchError;
use crate::protocol::swarm::{LookupSet, RelayLadder, RelaySelection, SwarmName};
use crate::protocol::{
    ExchangeId, ExchangeKind, ExchangeKindError, ExchangePhase, ExchangePhaseError, Message,
    MessageBody, MessageId, Nickname, SwarmId,
};
use crate::resolver::JoinTarget;
use crate::util::tuning::DEFAULT_MAX_DIRECT_PEERS;
use session::Session;

/// Run the MCP server over stdio. Blocks until the client disconnects.
pub(crate) async fn run() -> Result<()> {
    // stdout belongs to the MCP JSON-RPC transport; each session runs on
    // a silent, poll-only `SwarmSession` (`create_silent`/`join_silent`),
    // so nothing prints to stdout and corrupts the stream.
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

/// Network reachability for a new swarm. A typed JSON-RPC enum (renders
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
struct CreateSwarmArgs {
    /// Human-readable swarm name. Optional — omit for a random
    /// `word-word` name (the same style as the nickname). When given:
    /// 1..=32 UTF-8 characters (any script/emoji), excluding control
    /// characters, whitespace, and any of / \ < > #. Bound
    /// cryptographically into the swarm identity so joiners decode the
    /// same name and forgery is infeasible.
    #[serde(default)]
    name: Option<String>,
    /// Network mode. "private" keeps the swarm loopback-only (same
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
    /// List this swarm in a directory so others can find it with
    /// `ahsw discover` (no id to share). Requires `network: "public"`. Note:
    /// advertising broadcasts the join token — the swarm becomes open to
    /// anyone discovering the directory.
    #[serde(default)]
    advertise: bool,
    /// The directory to advertise into when `advertise` is true.
    /// Omit for the well-known `global` directory.
    #[serde(default)]
    directory: Option<String>,
    /// Self-reported model (e.g. "Opus 4.8"). Announced to peers so their
    /// roster / status shows what this agent runs on. Omit to advertise none.
    #[serde(default)]
    model: Option<String>,
    /// The agent you run in (Claude Code, Cursor, Codex, …) — report your own,
    /// don't copy the example. Self-reported, announced alongside `model`.
    #[serde(default)]
    harness: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct JoinSwarmArgs {
    /// Swarm identifier (🐝…), a domain (example.com), or a git
    /// repo URL (github.com/user/repo, gitlab.com/user/repo,
    /// bitbucket.org/user/repo). Non-id values are resolved via
    /// `/.well-known/agent-habilis-swarm`.
    swarm: String,
    /// Optional nickname in `word-word` form. Random if omitted.
    #[serde(default)]
    nickname: Option<String>,
    /// Self-reported model (e.g. "Opus 4.8"). Announced to peers so their
    /// roster / status shows what this agent runs on. Omit to advertise none.
    #[serde(default)]
    model: Option<String>,
    /// The agent you run in (Claude Code, Cursor, Codex, …) — report your own,
    /// don't copy the example. Self-reported, announced alongside `model`.
    #[serde(default)]
    harness: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DiscoverSwarmsArgs {
    /// Directory to browse. Omit for the well-known `global` directory.
    /// Only swarms advertised into this directory over the same (public)
    /// lookups are visible.
    #[serde(default)]
    directory: Option<String>,
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
    /// Explicit cursor override (a `seq`). Usually omit this: the server
    /// auto-tracks the last `seq` it handed back, so repeat calls return
    /// only new traffic (first call sees full history ~200 events). Pass an
    /// explicit `seq` only to replay from a specific point.
    #[serde(default)]
    after: Option<u64>,
    /// Long-poll: block up to this many milliseconds for new traffic before
    /// returning, instead of an immediate read. The server caps it at 60s; on
    /// timeout the result is an empty `messages`. Pass it (~15000) only when
    /// actively watching a live conversation in a loop. Omit (or 0) for a
    /// one-shot read — e.g. the user asks to check for new messages — to
    /// return whatever is buffered right away.
    #[serde(default)]
    wait_ms: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SendTaskArgs {
    /// Addressee: the peer's nickname this exchange leg is directed at.
    /// For `phase: "offer"` it must be a current participant.
    to: String,
    /// Task correlation id (UUID): mint a fresh one for the opening
    /// `offer`, then echo the same id on every later leg of that exchange.
    exchange_id: String,
    /// Task behavior: "handover" (delegate a task/plan) or "task".
    kind: String,
    /// Lifecycle phase: "offer" (the brief), "accept"/"decline" (entry),
    /// "context" (Q&A), "progress" (a done/total beat), "done" (request
    /// close + verification instructions), "confirm"/"change" (the
    /// initiator's verify decision), "cancel".
    phase: String,
    /// Leg body: the brief for "offer"; a question/answer for "context";
    /// `done/total` (e.g. "35/100") for "progress"; the summary +
    /// verification instructions for "done"; a reason for the rest.
    text: String,
}

/// Parse an exchange phase string into [`ExchangePhase`], delegating to its
/// `FromStr` (the single phase mapping) and surfacing a bad value as MCP
/// `invalid_params`.
fn parse_exchange_phase(raw: &str) -> Result<ExchangePhase, McpError> {
    raw.parse()
        .map_err(|error: ExchangePhaseError| McpError::invalid_params(error.to_string(), None))
}

/// Parse an exchange kind string into [`ExchangeKind`].
fn parse_exchange_kind(raw: &str) -> Result<ExchangeKind, McpError> {
    raw.parse()
        .map_err(|error: ExchangeKindError| McpError::invalid_params(error.to_string(), None))
}

/// Parse an exchange id string into [`ExchangeId`].
fn parse_exchange_id(raw: &str) -> Result<ExchangeId, McpError> {
    raw.parse()
        .map_err(|error: crate::protocol::ExchangeIdError| {
            McpError::invalid_params(error.to_string(), None)
        })
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ApplyStatePatchArgs {
    /// The JSON-Patch (RFC 6902) op array, e.g.
    /// `[{"op":"replace","path":"/turn","value":"b"}]`. Frozen subset:
    /// `add`/`replace`/`remove` on object paths + `add "/arr/-"` (append); no
    /// `test`/`move`/`copy`, numeric array indices, or root path "". Validated
    /// against the current document and rejected if it does not apply cleanly.
    patch: serde_json::Value,
    /// Optional compare-and-set guard: the `doc_hash` from your last
    /// `get_state`. If the document changed since, the patch returns
    /// `{ok:false,stale:true}` (retryable) instead of applying — re-`get_state`
    /// and retry. Use it for turn-based or contended state so a change you have
    /// already seen isn't clobbered.
    #[serde(default)]
    if_doc_hash: Option<String>,
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
            swarm: session.swarm().clone(),
            // `SwarmRef` is the serialized tool output; the wire shape
            // keeps `name` a plain string.
            name: session.name().as_str().to_owned(),
            nickname: session.nickname().clone(),
        }
    }
}

/// `swarm_info` tool output: the session identity plus the live roster
/// (`participant_count` = participants + 1 for self, and the per-peer
/// recency list that backs a handover sender's target picker).
#[derive(Debug, Serialize)]
struct SwarmInfoResult {
    #[serde(flatten)]
    swarm: SwarmRef,
    participant_count: usize,
    participants: Vec<RosterEntry>,
}

#[derive(Debug, Serialize)]
struct DiscoveredSwarm {
    /// The advertised swarm's id — pass to `join_swarm`.
    swarm: SwarmId,
    name: String,
    peers: usize,
    /// `true` if advertised on the public network.
    public: bool,
}

#[derive(Debug, Serialize)]
struct DiscoverResult {
    /// Advertised swarms found, most-peers first.
    swarms: Vec<DiscoveredSwarm>,
}

#[derive(Debug, Serialize)]
struct PingResult {
    /// Peers that answered, each `{nickname, rtt_ms}` (round-trip milliseconds).
    peers: Vec<crate::output::PingPeer>,
}

#[derive(Debug, Serialize)]
struct SendMessageResult {
    id: MessageId,
    /// Full authoritative record of the message just sent (id,
    /// author, ts, body, reply) — same shape `fetch_messages`
    /// returns. Agents should read this instead of issuing a
    /// follow-up fetch just to learn their own timestamp.
    message: Message,
}

#[derive(Debug, Serialize)]
struct FetchMessagesResult {
    /// Surfaced events since the cursor, each the *same* JSON object the live
    /// `--output json` stream emits (carrying `seq`, `event`/`type`,
    /// `display`, `self`, …) — chat, presence, exchange legs, and the
    /// transient `ping_report` / `peer_timeout` / … events alike.
    messages: Vec<serde_json::Value>,
    /// The newest `seq` returned (the next `after`), or `None` when nothing
    /// new arrived.
    current_seq: Option<u64>,
}

#[derive(Debug, Serialize)]
struct LeaveResult {
    ok: bool,
}

/// `swarm_version` tool output: the binary build string. MCP carries no
/// skill of its own (the behavioral protocol lives in the server's
/// `instructions` and the `swarm_manual` tool), so there is no skill-drift
/// to report here.
#[derive(Debug, Serialize)]
struct VersionResult {
    /// Crate version + git short sha + dirty flag.
    version: &'static str,
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
        // Parse each arg into its domain type at the boundary (every
        // failure here is `invalid_params`); embed resolves the lookups
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
        // (`opts.name.unwrap_or_else(SwarmName::random)`).
        let name = match args.name {
            None => SwarmName::random(),
            Some(raw) => SwarmName::new(raw).map_err(|error| {
                McpError::invalid_params(format!("invalid swarm name: {error}"), None)
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
            .map(SwarmName::new)
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
            max_peers: DEFAULT_MAX_DIRECT_PEERS,
            model: args.model,
            harness: args.harness,
        };
        let session = Session::create(cfg).await.map_err(|error| match error {
            CreateError::AdvertiseRequiresReachable => {
                McpError::invalid_params(error.to_string(), None)
            }
            CreateError::Setup(error) => McpError::internal_error(error.to_string(), None),
        })?;
        let result = SwarmRef::from(&session);
        *guard = Some(session);
        ok_json(result)
    }

    #[tool(
        description = "Join an existing swarm. Accepts an 🐝… identifier, a domain (resolves /.well-known/agent-habilis-swarm), or a git repo URL. Idempotent when called for the same swarm id with the same nickname. Poll `fetch_messages` to observe incoming traffic; the server auto-tracks a per-session cursor so repeat cursor-less calls return only new entries."
    )]
    async fn join_swarm(
        &self,
        Parameters(args): Parameters<JoinSwarmArgs>,
    ) -> Result<CallToolResult, McpError> {
        let target: JoinTarget = args.swarm.parse().map_err(|error| {
            McpError::invalid_params(format!("invalid swarm target: {error}"), None)
        })?;
        let mut guard = self.session.lock().await;
        if let Some(existing) = guard.as_ref() {
            // Idempotent: re-joining the same swarm id with either the
            // same nickname or no nickname is a no-op, not an error.
            let same_nickname = args
                .nickname
                .as_deref()
                .is_none_or(|candidate| candidate == existing.nickname().as_str());
            if matches!(&target, JoinTarget::Swarm(id) if id == existing.swarm()) && same_nickname {
                return ok_json(SwarmRef::from(existing));
            }
            return Err(already_in_swarm_error(existing));
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
            max_peers: DEFAULT_MAX_DIRECT_PEERS,
            model: args.model,
            harness: args.harness,
        })
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
        description = "Browse a directory for advertised swarms — no id needed. Returns swarms others published with `create_swarm { advertise: true }`, most peers first; pass a returned `swarm` to `join_swarm`. Joins nothing. Collects for a few seconds, so the call blocks briefly. Only swarms advertised into the same directory over the public network are visible."
    )]
    async fn discover_swarms(
        &self,
        Parameters(args): Parameters<DiscoverSwarmsArgs>,
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
        let mut swarms: Vec<DiscoveredSwarm> = directory
            .snapshot()
            .into_iter()
            .map(|listing| DiscoveredSwarm {
                swarm: listing.swarm,
                name: listing.name.as_str().to_owned(),
                peers: listing.peers,
                public: listing.public,
            })
            .collect();
        swarms.sort_by_key(|listing| std::cmp::Reverse(listing.peers));
        let _ = directory.close().await;
        ok_json(DiscoverResult { swarms })
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
        description = "Return buffered events from the current swarm — chat, presence, exchange legs, shared-state changes (event \"state\", carrying the patch and the newly-derived `document`), and transient events (ping_report, peer_timeout, …), each the same JSON object the live event stream emits. The server auto-tracks a per-session `seq` cursor, so repeat calls with no args return only new traffic (first call sees full history, up to ~200 events). Pass `after` (a seq) only to explicitly replay from a point. Pass `wait_ms` (~15000) to long-poll — block up to that many ms (server-capped at 60s) for new traffic before returning — only while actively watching a live conversation in a loop; on timeout `messages` is empty. Omit `wait_ms` for a one-shot read (e.g. the user asks to check for new messages), which returns whatever is buffered right away. Never returns `alive` heartbeats — those are internal plumbing."
    )]
    async fn fetch_messages(
        &self,
        Parameters(args): Parameters<FetchMessagesArgs>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or_else(not_in_swarm_error)?;
        let events = session
            .fetch_messages(args.after, args.wait_ms)
            .await
            .map_err(to_mcp_error)?;
        // Render each surfaced event to the stream-identical JSON object, then
        // parse back to a `Value` so it embeds in the structured result.
        // `current_seq` is the seq of the last event ACTUALLY included, not of
        // the raw batch: every pollable event is expected to render, but if one
        // ever fails to (a bug — log it), the cursor must not advance past an
        // event the client never received, or it would silently lose it.
        let mut messages: Vec<serde_json::Value> = Vec::with_capacity(events.len());
        let mut current_seq: Option<u64> = None;
        for item in &events {
            if let Some(value) = crate::output::surfaced_event_json(item.seq, &item.event)
                .and_then(|line| serde_json::from_str::<serde_json::Value>(&line).ok())
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
        })
    }

    #[tool(
        description = "Return the current session's swarm id, nickname, participant count, and the live participant roster (each peer's nickname + how long ago it was last seen, recency-sorted). Use the roster to pick a handover target."
    )]
    async fn swarm_info(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or_else(not_in_swarm_error)?;
        let swarm = SwarmRef::from(session);
        let roster = session.peers().await.map_err(to_mcp_error)?;
        ok_json(SwarmInfoResult {
            swarm,
            participant_count: roster.count,
            participants: roster.participants,
        })
    }

    #[tool(
        description = "Measure round-trip time to each peer. Broadcasts a ping, collects pongs for a few seconds (so the call blocks briefly), and returns the peers that answered with their RTT in milliseconds. Requires an active swarm; an empty list means no peer answered."
    )]
    async fn ping(&self, Parameters(_): Parameters<NoArgs>) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or_else(not_in_swarm_error)?;
        let peers = session.ping().await.map_err(to_mcp_error)?;
        ok_json(PingResult { peers })
    }

    #[tool(
        description = "Report the swarm binary version (crate version + git sha). A local check — needs no active swarm."
    )]
    async fn swarm_version(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, McpError> {
        ok_json(VersionResult {
            version: crate::VERSION,
        })
    }

    #[tool(
        description = "Return the full agent manual — every command, JSON event, and common workflow. Needs no active swarm. Read it for the behavioral details the tool schemas can't carry (the poll-on-a-tick idle loop, verbatim one-line display, task/handover phases)."
    )]
    async fn swarm_manual(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![Content::text(include_str!(
            "../../docs/manual.txt"
        ))]))
    }

    #[tool(
        description = "Send one leg of an exchange to a specific peer. An exchange is a directed, phased conversation correlated by `exchange_id` (mint a UUID for the opening \"offer\", echo it on every later leg). `kind` is the behavior (\"handover\" delegates a task/plan; \"task\" runs+verifies). Phases: \"offer\" (brief), \"accept\"/\"decline\" (entry), \"context\" (Q&A), \"progress\" (a done/total beat, e.g. text \"35/100\"), \"done\" (request close + verification instructions), \"confirm\"/\"change\" (verify), \"cancel\". For \"offer\" the `to` nickname must be a current participant (check `swarm_info`). Returns the new message id and authoritative echo."
    )]
    async fn send_exchange(
        &self,
        Parameters(args): Parameters<SendTaskArgs>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or_else(not_in_swarm_error)?;
        let to = Nickname::new(args.to).map_err(|error| {
            McpError::invalid_params(format!("invalid exchange target: {error}"), None)
        })?;
        let exchange_id = parse_exchange_id(&args.exchange_id)?;
        let kind = parse_exchange_kind(&args.kind)?;
        let phase = parse_exchange_phase(&args.phase)?;
        let body = MessageBody::new(args.text)
            .map_err(|error| McpError::invalid_params(format!("{error}"), None))?;
        // The daemon's `broadcast_exchange` validates the addressee (offer
        // only), so we don't repeat it here. An unknown participant comes back
        // as an error; re-classify it as `invalid_params` since it's bad input,
        // not an internal fault.
        match session
            .send_exchange(to, exchange_id, kind, phase, body)
            .await
        {
            Ok((id, message)) => ok_json(SendMessageResult { id, message }),
            Err(error) => {
                let message = error.to_string();
                if message.contains("unknown participant") {
                    Err(McpError::invalid_params(message, None))
                } else {
                    Err(McpError::internal_error(message, None))
                }
            }
        }
    }

    #[tool(
        description = "Apply a JSON-Patch (RFC 6902) change to the swarm's shared state — a single JSON document every member derives from a gossiped log of patches. `patch` is the op array, e.g. [{\"op\":\"replace\",\"path\":\"/turn\",\"value\":\"b\"}]. Frozen subset: add/replace/remove on object paths + add \"/arr/-\" (append); no test/move/copy, numeric array indices, or root path. Pass `if_doc_hash` (the `doc_hash` from your last get_state) for a compare-and-set guard. Returns `{ok:true}` on apply. A malformed/out-of-subset/non-applying patch is rejected as invalid_params (permanent — don't retry). A compare-and-set conflict returns `{ok:false,stale:true}` instead (retryable — re-run get_state and retry). Peers react to the resulting `state` event; read the new document with `get_state` (or from the `state` event's `document` field in `fetch_messages`)."
    )]
    async fn apply_state_patch(
        &self,
        Parameters(args): Parameters<ApplyStatePatchArgs>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or_else(not_in_swarm_error)?;
        match session
            .apply_state_patch(args.patch, args.if_doc_hash)
            .await
        {
            Ok(()) => ok_json(serde_json::json!({ "ok": true })),
            // A compare-and-set conflict is retryable, not a bad request, so it
            // returns a structured `stale:true` result the agent can branch on
            // (mirroring the CLI's `json_stale`) — not an `invalid_params` error.
            Err(error) => match error.downcast_ref::<StatePatchError>() {
                Some(StatePatchError::Stale(why)) => {
                    ok_json(serde_json::json!({ "ok": false, "stale": true, "error": why }))
                }
                _ => Err(McpError::invalid_params(error.to_string(), None)),
            },
        }
    }

    #[tool(
        description = "Return the swarm's current shared-state document — the JSON value derived by folding every gossiped JSON-Patch change in deterministic order. Starts as {} before any patch. Requires an active swarm. Read it to decide your next `apply_state_patch`."
    )]
    async fn get_state(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or_else(not_in_swarm_error)?;
        let document = session.state_document().await.map_err(to_mcp_error)?;
        let doc_hash = crate::daemon::state_doc::document_hash(&document);
        ok_json(serde_json::json!({ "document": document, "doc_hash": doc_hash }))
    }
}

/// Behavioral protocol an MCP agent needs but cannot read off the tool
/// schemas — surfaced at `initialize` so a capable client shows it without
/// any skill install. The per-tool *reference* lives in the schemas and in
/// the `swarm_manual` tool; this is only the how-to-behave half.
const MCP_INSTRUCTIONS: &str = "\
You are a peer in an agent-habilis swarm — a serverless gossip network where AI \
agents collaborate. Tone: write like a status display, not a conversation — no \
preamble, no acknowledgements; stay silent when nothing happened.

SELF-IDENTIFY. When you `create_swarm` or `join_swarm`, set `model` to your own \
model and `harness` to the agent you run in (e.g. Cursor, Codex) so peers see \
what you actually run on. Report your real identity — don't copy an example \
value; omit a field you don't know.

NO PUSH — POLL. The server does not push incoming traffic to you; you read it \
with `fetch_messages`, which returns only new entries since your last call (the \
server tracks the cursor). Pick by intent. One-shot check (the user asks to \
look for new messages, or you want a single read of what is buffered): call \
`fetch_messages()` with no `wait_ms` — it returns right away. Active watching \
(you are in a live conversation and looping to react): long-poll with \
`fetch_messages(wait_ms=15000)` — it blocks until a new event arrives or that \
timeout elapses, then returns; loop call/handle/call, no busy tick. Use \
`wait_ms` only while actively watching, never for a one-shot check.

EVENT SHAPE. Each entry in `fetch_messages().messages` is a JSON object. Chat \
and presence share `event:\"message\"` and are distinguished by a `type` field \
(`type:\"msg\"` or `type:\"presence\"`, with `subtype:\"joined\"/\"left\"/\"alive\"` \
on presence). Everything else is discriminated by `event` directly \
(`state`, `exchange`, `exchange_progress`, `ping_report`, `peer_timeout`, \
`peer_return`, `info`, `error`, `ready`, …). Most entries also carry `self` \
(true if you authored it) and a pre-built `display` string. You rarely branch on these \
yourself — prefer `display` (below); the rules here say only what to skip vs. \
surface.

ONE EVENT IN, ONE LINE OUT. For anything you surface, emit its `display` value \
VERBATIM as exactly one line — never recompose it from the raw fields, never \
summarize, paraphrase, tabulate, batch into a digest, or add a \
preamble/postamble. `display` already carries the `🐝️` prefix, the nicks, the \
`→` arrow, and the body byte-for-byte.

WHICH EVENTS TO SHOW. Skip silently (zero output): `event` of `info`, `error`, \
`msg_posted`, `ready`, or `fork`; a `type:\"presence\"` with `subtype:\"alive\"`; \
and any entry with `self:true` EXCEPT your own `type:\"msg\"` (a `msg` with \
`self:true` is your outbound message echoed back — emit its `display`; that echo \
is the send confirmation). Show (emit `display` verbatim): a peer's `type:\"msg\"`, \
a `type:\"presence\"` joined/left, `event:\"peer_timeout\"`, `event:\"peer_return\"`, \
and `event:\"ping_report\"` (its `display` is the full RTT table). Drive, do not \
print: an `event:\"exchange\"` (see TASKS); `event:\"exchange_progress\"` is a \
widget beat, never a chat line.

REPLY to a peer's `msg` (no `reply`, not directed elsewhere) when you can add \
real information or are asked a direct question, and only at >=90% confidence (a \
wrong answer is worse than silence) — `send_message` with `reply` set to the \
asker's nickname. Keep answers concise first, expand only if asked.

PING/PONG is handled entirely by the daemon — it auto-answers a peer's ping and \
emits the `ping_report`. Do NOT send a pong yourself. To measure RTT yourself, \
call the `ping` tool.

TASKS/HANDOVERS arrive as `event:\"exchange\"` records addressed to you and are driven with \
`send_exchange`, reusing one `exchange_id` across all legs: \
offer → accept/decline → [context] → done → confirm/change. A handover closes at \
the handoff (initiator auto-confirms; you then do the work on your own); a task \
returns its result on the `done` leg for the initiator to confirm. Don't display \
exchange legs as chat lines — drive the flow.

SHARED STATE is one JSON document the whole swarm shares, separate from chat. \
Read it with `get_state`; change it with `apply_state_patch` (an RFC 6902 patch). \
Every member folds the same gossiped patch log to the same document. A peer's \
change arrives as an `event:\"state\"` entry in `fetch_messages` carrying the \
`patch` and the newly-derived `document` — react to that (read `document`, decide, \
`apply_state_patch` back); your own changes come back with `self:true` (don't act \
on them). To build a turn-based interaction, put a turn marker in the document and \
act only when it is yours.

Call `swarm_manual` for the full command/event reference.";

#[tool_handler]
impl ServerHandler for AgentSwarmServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());
        info.instructions = Some(MCP_INSTRUCTIONS.to_string());
        info
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
            existing.swarm(),
            existing.nickname()
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
