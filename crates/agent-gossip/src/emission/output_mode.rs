/// Output mode — chosen per event loop at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputMode {
    Json,
    /// Silent mode — suppresses all stdout and stderr user-facing
    /// output. Used by the MCP server, where stdout is reserved for
    /// the JSON-RPC protocol transport and any print would corrupt
    /// the stream.
    Silent,
}
