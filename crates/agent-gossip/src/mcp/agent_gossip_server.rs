use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use habilis_network::protocol::{
    JoinTarget, LookupSet, MeshName, MessageBody, Nickname, RelayLadder,
};
use habilis_network::util::tuning::GOSSIP_ACTIVE_VIEW_CAPACITY;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ErrorData as McpError, ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use tokio::sync::Mutex;

use super::session::Session;
use super::{
    A2aCallArgs, ApplyMetaMergeArgs, ApplyStateMergeArgs, CreateMeshArgs, DiscoverMeshesArgs,
    DiscoverResult, DiscoveredMesh, FetchMessagesArgs, FetchMessagesResult, JoinMeshArgs,
    LeaveResult, MCP_INSTRUCTIONS, MeshInfoResult, MeshRef, NoArgs, PingResult, SendBroadcastArgs,
    SendMsgArgs, SendResult, TaskArtifactArgs, TaskStatusArgs, TopicMeshArgs, VersionResult,
    already_in_mesh_error, join_error_to_mcp, not_in_mesh_error, ok_json, parse_task_id,
    parse_task_state, rejoin_existing, to_mcp_error,
};
use crate::api::{
    A2aCallParams, CreateConfig, CreateError, Directory, JoinConfig, TaskArtifactParams,
    TaskStatusParams, TopicConfig,
};
use crate::cli::args::{lookup, resolved};
use crate::topic;

#[derive(Clone)]
pub(crate) struct AgentGossipServer {
    pub(crate) session: Arc<Mutex<Option<Session>>>,
}

impl AgentGossipServer {
    pub(crate) fn new() -> Self {
        Self {
            session: Arc::new(Mutex::new(None)),
        }
    }
}

#[tool_router(vis = "pub(crate)")]
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
        let relay_urls = if args.relay_urls.is_empty() {
            None
        } else {
            Some(
                RelayLadder::from_str(&args.relay_urls.join(",")).map_err(|error| {
                    McpError::invalid_params(format!("invalid relay ladder: {error}"), None)
                })?,
            )
        };
        let lookups = lookup::lookups_or_public(&args.lookup, args.advertise);
        let resolved = resolved::resolve(&lookups, relay_urls, &args.transport)
            .map_err(|error| McpError::invalid_params(error.to_string(), None))?;
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
        let config = CreateConfig {
            name,
            nickname,
            lookups: resolved.set,
            transport: resolved.transport,
            advertise: args.advertise,
            directory,
            max_peers: GOSSIP_ACTIVE_VIEW_CAPACITY,
            password: args.password,
        };
        let session = Session::create(config).await.map_err(|error| match error {
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
        description = "Join a public gossip derived deterministically from a shared string — no id to share. Anyone who calls topic_gossip with the same string joins the same gossip, on any machine (the string is hashed into the gossip seed; the name is derived, networking is always public, and payload can go through the relay when a direct path fails). The string is matched byte-for-byte after trimming whitespace, so pick an exact, agreed value. Idempotent when called for the same string with the same nickname. Poll `fetch_messages` to observe traffic."
    )]
    async fn topic_gossip(
        &self,
        Parameters(args): Parameters<TopicMeshArgs>,
    ) -> Result<CallToolResult, McpError> {
        let derived = topic::derive_topic_mesh(&args.string)
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
        let mut config = TopicConfig::new(args.string);
        config.nickname = nickname;
        let session = Session::topic(config).await.map_err(join_error_to_mcp)?;
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

        let mut directory = Directory::open(args.directory.as_deref(), LookupSet::default())
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
        description = "Return buffered events from the current gossip — chat, presence, task legs, shared-state changes (event \"state\", carrying the merge and the newly-derived `document`; a \"meta\" event carries the merge only, read the meta document with `get_meta`), and transient events (ping_report, peer_timeout, …), each the same JSON object the live event stream emits. The server auto-tracks a per-session `seq` cursor, so repeat calls with no args return only new traffic (first call sees full history, up to ~200 events). Pass `after` (a seq) only to explicitly replay from a point. Pass `long: true` to long-poll — park the read until new traffic arrives (server-capped at ~60s) — only while actively watching a live conversation in a loop; an empty `messages` at the deadline just means the window elapsed quietly, call again. Omit `long` for a one-shot read (e.g. the user asks to check for new messages), which returns whatever is buffered right away. Never returns `alive` heartbeats — those are internal plumbing."
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
        description = "Return the current session's gossip id, nickname, peer count, and the live peer roster (each peer's nickname + how long ago it was last seen, recency-sorted). peer_count is the active peers plus self; the roster additionally carries quiet peers (heartbeat-evicted, may return) flagged quiet:true, so it is longer than peer_count - 1 whenever one is quiet. Use the roster to pick a task target."
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
        description = "As the WORKER on a task you're serving, emit a TaskStatusUpdate (the A2A streaming plane). Use \"working\" to accept/resume, \"input-required\" to ask the initiator a question or (after `task_artifact`) request approval, \"completed\" to close it after the initiator approves, \"failed\"/\"canceled\" to end it. A directed task is created by `a2a_call` message/send (the worker mints the id and returns the Task); drive it forward with this. `note` becomes the status message (a question / summary / reason). Before you ask your own user something, send \"working\" with `waiting` set to the reason, so the initiator can tell a wait from a stall; the next status clears it."
    )]
    async fn task_status(
        &self,
        Parameters(args): Parameters<TaskStatusArgs>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or_else(not_in_mesh_error)?;
        let task_id = parse_task_id(&args.task_id)?;
        let state = parse_task_state(&args.state)?;
        crate::a2a::broadcast_params::WorkerLeg::parse(state, args.waiting.as_deref())
            .map_err(|_| McpError::invalid_params("waiting requires state \"working\"", None))?;
        match session
            .task_status(TaskStatusParams {
                task_id,
                state,
                note: args.note,
                waiting: args.waiting,
            })
            .await
        {
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
        description = "Call a peer's A2A server over gossip (request/response) and return its JSON-RPC response. The peer serves a SAFE subset of A2A v1.0 (PascalCase) methods: \"GetTask\" (params {id}), \"CancelTask\" (params {id}), \"SubscribeToTask\" (params {id}; snapshot + the worker's pushed frames) — all three only for a task you're a party to, and a task between that peer and someone else answers the same \"task not found\" as an id that does not exist; \"ListTasks\", which lists only the tasks you share with that peer, not its whole registry; \"mesh/state.get\", \"mesh/meta.get\"; and \"SendMessage\" (params {message: <A2A Message>}) directed at that peer — no taskId opens a task the worker mints and returns as {\"task\": <Task>}; a taskId is a follow-up. A peer caps how many live tasks any one caller may hold with it and refuses further opens past that, so treat a task as a resource to close rather than to accumulate. Global-state writes (mesh/state.merge, mesh/meta.merge) and broadcast SendMessage are refused. Blocks until the peer answers or times out; the result is the JSON-RPC response object ({result} or {error})."
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
        // Keyed on `error` itself, not `error.message`: the CLI exits non-zero
        // on any non-null `error` (`cli::a2a`), so keying on the nested field
        // made an error object without a message read as success here and
        // failure there.
        if !response["error"].is_null() {
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
        description = "Apply an RFC 7386 JSON Merge Patch to the gossip's `meta` channel — a second shared-state document beside `state`, byte-for-byte the same machinery, used for gossip metadata rather than the task. By convention agents self-report what they run on under `/peers/<nickname>`, e.g. {\"peers\":{\"swift-cedar\":{\"model\":\"Opus 4.8\",\"harness\":\"Claude Code\",\"host\":\"studio-mbp-01\",\"cwd\":\"~/src/app\"}}} (`host` is the machine's hostname, `cwd` your working directory with $HOME shortened to `~`). Same merge semantics as `apply_state_merge` — your own entry merges in without clobbering other peers; set a peer key to null to clear it. Peers react to the resulting `meta` event; read the new document with `get_meta`."
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
        description = "Return the gossip's current `meta`-channel document (the gossip-metadata counterpart of `get_state`) — the JSON value derived by folding every gossiped `meta` merge. Starts as {} before any merge. By convention holds `/peers/<nickname> = {model, harness, host, cwd}`. Requires an active gossip. Read it to decide your next `apply_meta_merge` or to see what peers run on. `absent` lists the `/peers` entries with no active member behind them: only a peer can retract its own entry, so one that died without leaving gracefully stays in the document forever, still reporting whatever status it last set. Treat those entries as history, not as candidates for work."
    )]
    async fn get_meta(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or_else(not_in_mesh_error)?;
        let document = session.meta_get().await.map_err(to_mcp_error)?;
        // Same rule as the CLI's `meta get`, from the same function, so the two
        // agent-facing surfaces cannot drift apart on what "absent" means.
        let roster = session.peers().await.map_err(to_mcp_error)?;
        let live: HashSet<&str> = roster
            .peers
            .iter()
            .filter(|entry| !entry.quiet)
            .map(|entry| entry.nickname.as_str())
            .collect();
        let absent = crate::a2a::card::absent_peers(&document, &live, session.nickname());
        ok_json(serde_json::json!({ "document": document, "absent": absent }))
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "rmcp's ServerHandler methods return futures, and tool_handler generates async fns that return ready values"
)]
#[tool_handler]
impl ServerHandler for AgentGossipServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());
        info.instructions = Some(MCP_INSTRUCTIONS.to_string());
        info
    }
}
