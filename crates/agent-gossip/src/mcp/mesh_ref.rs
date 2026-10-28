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
        MeshRef {
            gossip: session.mesh().clone(),
            // `MeshRef` is the serialized tool output; the wire shape
            // keeps `name` a plain string.
            name: session.name().as_str().to_owned(),
            nickname: session.nickname().clone(),
        }
    }
}
