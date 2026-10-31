use rmcp::schemars;
use serde::Deserialize;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct TopicMeshArgs {
    /// Any string. Hashed into a deterministic **public** gossip — anyone who
    /// joins with the same string lands in the same gossip, on any machine.
    /// Compared byte-for-byte after trimming surrounding whitespace (not
    /// lowercased), so `http://x` and `https://x` are different topics.
    pub(crate) string: String,
    /// Optional nickname in `word-word` form. Random if omitted.
    #[serde(default)]
    pub(crate) nickname: Option<String>,
}
