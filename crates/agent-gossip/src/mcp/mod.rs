//! MCP (Model Context Protocol) server mode.
//!
//! Runs as a stdio JSON-RPC server that AI clients (Codex, Cursor,
//! Claude Desktop, Claude Code) can spawn as a child process.
//! Exposes nineteen tools that wrap the existing mesh lifecycle:
//!
//! - `create_gossip`
//! - `join_gossip`
//! - `topic_gossip`
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

mod a2a_call_args;
mod agent_gossip_server;
mod already_in_mesh_error;
mod apply_meta_merge_args;
mod apply_state_merge_args;
mod create_mesh_args;
mod default_a2a_timeout_secs;
mod discover_meshes_args;
mod discover_result;
mod discovered_mesh;
mod fetch_messages_args;
mod fetch_messages_result;
mod join_error_to_mcp;
mod join_mesh_args;
mod leave_result;
mod mcp_instructions;
mod mesh_info_result;
mod mesh_ref;
mod no_args;
mod not_in_mesh_error;
mod ok_json;
mod parse_task_id;
mod parse_task_state;
mod ping_result;
mod rejoin_existing;
mod run;
mod send_broadcast_args;
mod send_msg_args;
mod send_result;
mod serialize_message_view;
mod session;
mod task_artifact_args;
mod task_status_args;
#[cfg(test)]
mod tests;
mod to_mcp_error;
mod topic_mesh_args;
mod version_result;

pub(crate) use self::a2a_call_args::A2aCallArgs;
pub(crate) use self::agent_gossip_server::AgentGossipServer;
pub(crate) use self::already_in_mesh_error::already_in_mesh_error;
pub(crate) use self::apply_meta_merge_args::ApplyMetaMergeArgs;
pub(crate) use self::apply_state_merge_args::ApplyStateMergeArgs;
pub(crate) use self::create_mesh_args::CreateMeshArgs;
pub(crate) use self::default_a2a_timeout_secs::default_a2a_timeout_secs;
pub(crate) use self::discover_meshes_args::DiscoverMeshesArgs;
pub(crate) use self::discover_result::DiscoverResult;
pub(crate) use self::discovered_mesh::DiscoveredMesh;
pub(crate) use self::fetch_messages_args::FetchMessagesArgs;
pub(crate) use self::fetch_messages_result::FetchMessagesResult;
pub(crate) use self::join_error_to_mcp::join_error_to_mcp;
pub(crate) use self::join_mesh_args::JoinMeshArgs;
pub(crate) use self::leave_result::LeaveResult;
pub(crate) use self::mcp_instructions::MCP_INSTRUCTIONS;
pub(crate) use self::mesh_info_result::MeshInfoResult;
pub(crate) use self::mesh_ref::MeshRef;
pub(crate) use self::no_args::NoArgs;
pub(crate) use self::not_in_mesh_error::not_in_mesh_error;
pub(crate) use self::ok_json::ok_json;
pub(crate) use self::parse_task_id::parse_task_id;
pub(crate) use self::parse_task_state::parse_task_state;
pub(crate) use self::ping_result::PingResult;
pub(crate) use self::rejoin_existing::rejoin_existing;
pub(crate) use self::run::run;
pub(crate) use self::send_broadcast_args::SendBroadcastArgs;
pub(crate) use self::send_msg_args::SendMsgArgs;
pub(crate) use self::send_result::SendResult;
pub(crate) use self::serialize_message_view::serialize_message_view;
pub(crate) use self::task_artifact_args::TaskArtifactArgs;
pub(crate) use self::task_status_args::TaskStatusArgs;
pub(crate) use self::to_mcp_error::to_mcp_error;
pub(crate) use self::topic_mesh_args::TopicMeshArgs;
pub(crate) use self::version_result::VersionResult;
