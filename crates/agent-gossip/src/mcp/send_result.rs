use habilis_network::protocol::MessageId;
use habilis_network::protocol::messages::Authored;
use serde::Serialize;

use super::serialize_message_view;

#[derive(Debug, Serialize)]
pub(crate) struct SendResult {
    pub(crate) id: MessageId,
    /// Full authoritative record of the frame just sent (id, author, ts,
    /// `to`, and `body` carrying the serialized A2A Message payload).
    /// Agents should read this instead of issuing a follow-up fetch just
    /// to learn their own timestamp.
    #[serde(serialize_with = "serialize_message_view")]
    pub(crate) message: Authored,
}
