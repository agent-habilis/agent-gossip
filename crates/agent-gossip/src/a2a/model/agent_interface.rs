use serde::{Deserialize, Serialize};

/// A2A v1.0 `AgentInterface`: one reachable endpoint for a binding. A member
/// always declares its gossip interface (the pubkey-addressed mesh endpoint);
/// a `--a2a-serve` daemon additionally declares its localhost JSON-RPC one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AgentInterface {
    pub url: String,
    pub protocol_binding: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    pub protocol_version: String,
}
