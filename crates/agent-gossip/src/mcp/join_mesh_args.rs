use rmcp::schemars;
use serde::Deserialize;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct JoinMeshArgs {
    /// Gossip identifier (…). A shared string derives its own public
    /// gossip — use the `topic` command — and is not a valid join target.
    pub(crate) gossip: String,
    /// Optional nickname in `word-word` form. Random if omitted.
    #[serde(default)]
    pub(crate) nickname: Option<String>,
    /// Password for a password-protected gossip id. Never prompts — required
    /// exactly when the id is password-protected, rejected otherwise.
    #[serde(default)]
    pub(crate) password: Option<String>,
}
