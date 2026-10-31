use rmcp::schemars;
use serde::Deserialize;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct ApplyMetaMergeArgs {
    /// The RFC 7386 JSON Merge Patch for the **meta** channel — gossip metadata,
    /// by convention `/peers/<nickname> = {"model":…,"harness":…,"host":…,"cwd":…}`
    /// self-reported by each agent (`host` is the machine's hostname, `cwd` the
    /// working directory with $HOME shortened to `~`), e.g.
    /// `{"peers":{"swift-cedar":{"model":"Opus 4.8","harness":"Claude Code","host":"studio-mbp-01","cwd":"~/src/app"}}}`.
    /// Same merge semantics as `apply_state_merge`: each key merges in, `null`
    /// deletes — so reporting your own entry never clobbers another peer's.
    pub(crate) merge: serde_json::Value,
}
