use habilis_network::protocol::MeshId;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub(crate) struct DiscoveredMesh {
    /// The advertised gossip's id — pass to `join_gossip`.
    pub(crate) gossip: MeshId,
    pub(crate) name: String,
    pub(crate) peers: usize,
    /// `true` if advertised on the public network.
    pub(crate) public: bool,
    /// `true` if password-protected — `join_gossip` needs the password.
    pub(crate) password: bool,
}
