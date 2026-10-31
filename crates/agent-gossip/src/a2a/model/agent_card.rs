use serde::{Deserialize, Serialize};

use super::{AgentCapabilities, AgentInterface, AgentSkill};

/// The self-description a peer publishes: interfaces, capabilities, and skills.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AgentCard {
    /// Display name of the agent.
    pub name: String,
    /// What the agent is for.
    pub description: String,
    /// Every endpoint the agent reaches over; the gossip one is always present.
    pub supported_interfaces: Vec<AgentInterface>,
    /// Version of the agent software, not of the A2A spec.
    pub version: String,
    /// Link to human-readable docs for the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation_url: Option<String>,
    /// The optional protocol features the agent supports.
    pub capabilities: AgentCapabilities,
    /// Media types a skill accepts unless it says otherwise.
    pub default_input_modes: Vec<String>,
    /// Media types a skill returns unless it says otherwise.
    pub default_output_modes: Vec<String>,
    /// The skills the agent offers.
    pub skills: Vec<AgentSkill>,
    /// Auth schemes, as JSON; `None` when the agent asks for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security_schemes: Option<serde_json::Value>,
}
