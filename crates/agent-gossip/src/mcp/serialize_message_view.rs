use anyhow::Result;
use habilis_network::protocol::messages::Authored;
use serde::Serialize;

use crate::a2a::ipc_command;

/// The tool output reports the sent frame as the IPC record does: a read-only view, not a sender.
pub(crate) fn serialize_message_view<S: serde::Serializer>(
    message: &Authored,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    ipc_command::message_record(message).serialize(serializer)
}
