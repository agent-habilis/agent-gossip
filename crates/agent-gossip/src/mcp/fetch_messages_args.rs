use rmcp::schemars;
use serde::Deserialize;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct FetchMessagesArgs {
    /// Explicit cursor override (a `seq`). Usually omit this: the server
    /// auto-tracks the last `seq` it handed back, so repeat calls return
    /// only new traffic (first call sees full history ~200 events). Pass an
    /// explicit `seq` only to replay from a specific point.
    #[serde(default)]
    pub(crate) after: Option<u64>,
    /// Long-poll: park the read until new traffic arrives, up to ~60s. An
    /// empty `messages` at the deadline just means the window elapsed
    /// quietly — call again. Pass `true` only when actively watching a live
    /// conversation in a loop. Omit for a one-shot read — e.g. the user asks
    /// to check for new messages — to return whatever is buffered right away.
    #[serde(default)]
    pub(crate) long: bool,
}
