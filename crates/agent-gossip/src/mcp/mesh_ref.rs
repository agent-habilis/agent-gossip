use habilis_network::protocol::{MeshId, Nickname};
use serde::Serialize;

use super::session::Session;

#[derive(Debug, Serialize)]
pub(crate) struct MeshRef {
    pub(crate) gossip: MeshId,
    pub(crate) name: String,
    pub(crate) nickname: Nickname,
    /// Our author key in hex: the slot of our entry in the meta document. An MCP client has no
    /// `ready` output to read it from.
    pub(crate) pubkey: String,
}

impl From<&Session> for MeshRef {
    fn from(session: &Session) -> Self {
        // `MeshRef` is the serialized tool output; the wire shape
        // keeps `name` a plain string.
        let name: String = session.core.name().as_str().to_owned();
        let pubkey = session.own_key().to_string();
        MeshRef {
            gossip: session.core.mesh_id().clone(),
            name,
            nickname: session.core.nickname().clone(),
            pubkey,
        }
    }
}
