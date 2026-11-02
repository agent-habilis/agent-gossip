use rmcp::schemars;
use serde::Deserialize;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct SendMsgArgs {
    /// The peer's nickname.
    pub(crate) to: String,
    /// Message body. UTF-8; newlines/tabs allowed, other control
    /// characters rejected. Seen only by you and `to`.
    pub(crate) text: String,
}
