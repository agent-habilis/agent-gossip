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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pubkey: Option<String>,
}

impl From<&Session> for MeshRef {
    fn from(session: &Session) -> Self {
        // `MeshRef` is the serialized tool output; the wire shape
        // keeps `name` a plain string.
        let name: String = session.name().as_str().to_owned();
        let pubkey: Option<String> = session.own_key().map(|key| key.to_string());
        MeshRef {
            gossip: session.mesh().clone(),
            name,
            nickname: session.nickname().clone(),
            pubkey,
        }
    }
}
