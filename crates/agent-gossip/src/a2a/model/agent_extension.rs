use serde::{Deserialize, Serialize};

/// One extension an agent card declares, found by its URI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AgentExtension {
    /// The URI that identifies the extension, such as `EXT_MESH_STATE`.
    pub uri: String,
    /// What the extension does for this agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `Some(true)` when a client must support the extension to talk to this agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    /// Extension-specific JSON settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}
