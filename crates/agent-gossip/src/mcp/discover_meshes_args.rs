use rmcp::schemars;
use serde::Deserialize;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct DiscoverMeshesArgs {
    /// Directory to browse. Omit for the well-known `global` directory.
    /// Only gossips advertised into this directory over the same (public)
    /// lookups are visible.
    #[serde(default)]
    pub(crate) directory: Option<String>,
}
