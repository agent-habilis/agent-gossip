use habilis_network::embed::RosterEntry;
use serde::Serialize;

use super::MeshRef;

/// `gossip_info` tool output: the session identity plus the live roster
/// (`peer_count` = peers + 1 for self, and the per-peer
/// recency list that backs a task sender's target picker).
#[derive(Debug, Serialize)]
pub(crate) struct MeshInfoResult {
    #[serde(flatten)]
    pub(crate) mesh: MeshRef,
    pub(crate) peer_count: usize,
    pub(crate) peers: Vec<RosterEntry>,
}
