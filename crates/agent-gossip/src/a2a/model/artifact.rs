use serde::{Deserialize, Serialize};

use super::Part;

/// A task output made of parts, such as a result file or document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Artifact {
    /// Unique within the task; a later update with the same id replaces or extends it.
    pub artifact_id: String,
    /// The content of the artifact, in order.
    pub parts: Vec<Part>,
    /// Short label for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// What the artifact holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// URIs of the extensions that apply to this artifact.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    /// Free-form JSON that rides along with the artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}
