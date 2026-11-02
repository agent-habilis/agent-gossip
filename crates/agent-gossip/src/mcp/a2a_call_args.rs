use rmcp::schemars;
use serde::Deserialize;

use super::default_a2a_timeout_secs;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct A2aCallArgs {
    /// The peer to call (a current peer).
    pub(crate) to: String,
    /// The A2A JSON-RPC method. The peer serves a safe subset:
    /// "`GetTask`", "`CancelTask`" (both only for a task you're a party to),
    /// "`ListTasks`" (only the tasks you share with that peer),
    /// "mesh/state.get", "mesh/meta.get", and "`SendMessage`"
    /// directed at the peer (it ingests the message and returns the Task).
    pub(crate) method: String,
    /// The JSON-RPC params object (default `{}`).
    #[serde(default)]
    pub(crate) params: serde_json::Value,
    /// How long to wait for the peer's response, in seconds.
    #[serde(default = "default_a2a_timeout_secs")]
    pub(crate) timeout_secs: u64,
}
