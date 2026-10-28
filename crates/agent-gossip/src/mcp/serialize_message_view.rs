use anyhow::Result;
use serde::Serialize;

/// The tool output reports the sent frame as the IPC record does: a read-only view, not a sender.
pub(crate) fn serialize_message_view<S: serde::Serializer>(
    message: &habilis_network::protocol::message::Authored,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    crate::a2a::ipc_command::message_record(message).serialize(serializer)
}
