use serde::{Deserialize, Serialize};

use super::AgentExtension;

/// The optional protocol features an agent declares in its card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AgentCapabilities {
    /// `Some(true)` when the agent streams task events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streaming: Option<bool>,
    /// `Some(true)` when the agent sends push notifications.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_notifications: Option<bool>,
    /// The extensions the agent supports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<AgentExtension>,
    /// `Some(true)` when a fuller card is available to an authenticated client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extended_agent_card: Option<bool>,
}
