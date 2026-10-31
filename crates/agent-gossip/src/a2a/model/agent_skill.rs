use serde::{Deserialize, Serialize};

/// One skill an agent card advertises.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AgentSkill {
    /// Stable identifier of the skill.
    pub id: String,
    /// Short name for display.
    pub name: String,
    /// What the skill does.
    pub description: String,
    /// Keywords that help a client pick a skill.
    pub tags: Vec<String>,
    /// Example prompts that exercise the skill.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
    /// Media types the skill accepts; overrides the card default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_modes: Vec<String>,
    /// Media types the skill returns; overrides the card default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_modes: Vec<String>,
}
