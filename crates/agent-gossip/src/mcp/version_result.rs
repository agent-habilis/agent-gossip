use serde::Serialize;

/// `gossip_version` tool output: the binary build string. MCP carries no
/// skill of its own (the behavioral protocol lives in the server's
/// `instructions` and the `gossip_manual` tool), so there is no skill-drift
/// to report here.
#[derive(Debug, Serialize)]
pub(crate) struct VersionResult {
    /// Crate version + git short sha + dirty flag.
    pub(crate) version: &'static str,
}
