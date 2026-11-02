use anyhow::Result;
use rmcp::ServiceExt;
use rmcp::transport::stdio;

use super::AgentGossipServer;

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
