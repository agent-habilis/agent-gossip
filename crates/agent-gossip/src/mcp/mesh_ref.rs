use habilis_network::protocol::{MeshId, Nickname};
use serde::Serialize;

use super::session::Session;

#[derive(Debug, Serialize)]
pub(crate) struct MeshRef {
    pub(crate) gossip: MeshId,
    pub(crate) name: String,
    pub(crate) nickname: Nickname,
}

impl From<&Session> for MeshRef {
    fn from(session: &Session) -> Self {
        // `MeshRef` is the serialized tool output; the wire shape
        // keeps `name` a plain string.
        let name: String = session.name().as_str().to_owned();
        MeshRef {
            gossip: session.mesh().clone(),
            name,
            nickname: session.nickname().clone(),
        }
    }
}
