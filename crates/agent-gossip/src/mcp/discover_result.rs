use serde::Serialize;

use super::DiscoveredMesh;

#[derive(Debug, Serialize)]
pub(crate) struct DiscoverResult {
    /// Advertised gossips found, most-peers first.
    pub(crate) gossips: Vec<DiscoveredMesh>,
}
