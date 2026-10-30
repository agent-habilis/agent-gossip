use serde::{Deserialize, Serialize};

/// A2A v1.0 `AgentInterface`: one reachable endpoint for a binding.
///
/// A member
/// always declares its gossip interface (the pubkey-addressed mesh endpoint);
/// a `--a2a-serve` daemon additionally declares its localhost JSON-RPC one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AgentInterface {
    /// Where to reach the endpoint: a pubkey address for gossip, a localhost URL for JSON-RPC.
    pub url: String,
    /// Names the binding the endpoint speaks, such as `GOSSIP_BINDING`.
    pub protocol_binding: String,
    /// Tenant to put on requests; `None` when the agent has no tenants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// The A2A revision the endpoint speaks; see `PROTOCOL_VERSION`.
    pub protocol_version: String,
}
