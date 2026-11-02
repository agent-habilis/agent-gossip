use rmcp::schemars;
use serde::Deserialize;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct SendBroadcastArgs {
    /// Message body. UTF-8; newlines/tabs allowed, other control
    /// characters rejected. Broadcast to the whole gossip — use
    /// `send_msg` to reach one peer privately.
    pub(crate) text: String,
}
