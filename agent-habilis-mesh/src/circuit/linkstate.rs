//! Link-state dissemination model: a peer's advertised **link-vector** (the
//! links it measures itself, gossiped as a global change) and the assembly of
//! many peers' vectors into the routing [`Graph`].
//!
//! Each origin advertises only its *own* outbound links, carrying a monotonic
//! `seq` so a newer vector supersedes an older one (last-writer-wins per
//! origin). Every node folds the freshest vector from each origin into one
//! metric-weighted graph and runs Dijkstra locally — no per-message discovery.

use std::collections::{HashMap, HashSet};

use iroh::EndpointId;

use super::graph::Graph;
use super::metric::LinkMetric;
use super::telemetry::NeighborProfile;

/// Build this node's link-vector for emission: our own X25519 key (so peers can
/// onion-seal circuits through us) plus one link to each **direct neighbour**,
/// carrying its currently-measured metric (a default until the telemetry probe
/// is wired). `seq` orders our successive advertisements so a receiver keeps
/// only the freshest.
#[derive(Clone, Copy)]
pub(crate) struct SelfVectorParams<'a> {
    pub(crate) origin: EndpointId,
    pub(crate) seq: u64,
    pub(crate) seal_key: [u8; 32],
    pub(crate) neighbors: &'a HashSet<EndpointId>,
    pub(crate) telemetry: &'a HashMap<EndpointId, NeighborProfile>,
}

pub(crate) fn self_vector(params: SelfVectorParams<'_>) -> LinkVector {
    let SelfVectorParams {
        origin,
        seq,
        seal_key,
        neighbors,
        telemetry,
    } = params;
    // Each link advertises our own measured cost to that neighbour (from ping
    // RTT / delivery), or the default profile's cost until we've probed it.
    let default = NeighborProfile::default().metric();
    let links = neighbors
        .iter()
        .map(|neighbor| {
            let metric = telemetry
                .get(neighbor)
                .map_or(default, NeighborProfile::metric);
            (*neighbor, metric)
        })
        .collect();
    LinkVector {
        origin,
        seq,
        seal_key,
        links,
    }
}

/// One peer's advertised view of its **own** outbound links — the gossiped
/// payload. `seq` orders successive vectors from the same `origin`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LinkVector {
    pub(crate) origin: EndpointId,
    pub(crate) seq: u64,
    /// The origin's X25519 public key — so an initiator can onion-seal a circuit
    /// hop through this node without a separate card lookup.
    pub(crate) seal_key: [u8; 32],
    pub(crate) links: Vec<(EndpointId, LinkMetric)>,
}

impl LinkVector {
    /// Serialize for the `LinkState` frame body (compact JSON).
    ///
    /// # Errors
    /// Propagates a `serde_json` failure (unreachable for these plain fields).
    pub(crate) fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }

    /// Parse a received `LinkState` frame body.
    ///
    /// # Errors
    /// Fails on malformed JSON or a shape mismatch.
    pub(crate) fn from_json(raw: &str) -> serde_json::Result<Self> {
        serde_json::from_str(raw)
    }

    /// Build a vector from raw `(neighbour, cost)` links — the injection point
    /// for the testkit `MeshSession::inject_link_vector`, which stands up a
    /// synthetic circuit topology over a live mesh (live convergence forms no
    /// usable edges in a rendezvous-bootstrapped mesh). Keeps [`LinkMetric`]
    /// crate-internal.
    #[cfg(feature = "adversarial")]
    #[must_use]
    pub fn from_raw(
        origin: EndpointId,
        seq: u64,
        seal_key: [u8; 32],
        links: Vec<(EndpointId, u32)>,
    ) -> Self {
        Self {
            origin,
            seq,
            seal_key,
            links: links
                .into_iter()
                .map(|(id, cost)| (id, LinkMetric(cost)))
                .collect(),
        }
    }
}

/// The received link-vectors, kept freshest-per-origin — the routing table's
/// raw material. `graph()` folds them into the directed graph on demand.
#[derive(Debug, Default)]
pub struct LinkStateStore {
    vectors: HashMap<EndpointId, LinkVector>,
}

/// A node's-eye view of the routing graph, ready to serialize to JSON.
#[derive(Debug, serde::Serialize)]
pub struct Topology {
    /// This node's own endpoint id (hex) — the graph's "you".
    pub(crate) self_id: String,
    pub(crate) edges: Vec<TopologyEdge>,
}

/// One directed, metric-labelled edge in the [`Topology`].
#[derive(Debug, serde::Serialize)]
pub(crate) struct TopologyEdge {
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) metric: u32,
}

impl LinkStateStore {
    /// Ingest a received vector, keeping it only if it is newer (`seq`) than what
    /// we already hold for that origin. Returns whether the store changed, so the
    /// caller knows when to rebuild the graph.
    pub fn ingest(&mut self, vector: LinkVector) -> bool {
        match self.vectors.get(&vector.origin) {
            Some(existing) if existing.seq >= vector.seq => false,
            _ => {
                self.vectors.insert(vector.origin, vector);
                true
            }
        }
    }

    /// Drop an origin's advertised links (e.g. when the peer leaves the mesh).
    /// Returns whether anything was removed.
    pub(crate) fn remove(&mut self, origin: EndpointId) -> bool {
        self.vectors.remove(&origin).is_some()
    }

    /// The X25519 key an origin advertised, for onion-sealing a circuit hop
    /// through it.
    pub(crate) fn seal_key(&self, origin: EndpointId) -> Option<[u8; 32]> {
        self.vectors.get(&origin).map(|vector| vector.seal_key)
    }

    /// Up to `max_paths` **node-disjoint** source-routes from `src` to `dst`,
    /// shortest first — the warm-pool / retry candidates. Each is the ordered
    /// hops after `src` (up to and including `dst`) paired with each hop's X25519
    /// key. A path is dropped if any hop hasn't advertised its key yet (we can't
    /// seal a layer we lack a key for); the returned list may thus be shorter
    /// than `max_paths` or empty.
    pub(crate) fn circuit_paths(
        &self,
        src: EndpointId,
        dst: EndpointId,
        max_paths: usize,
    ) -> Vec<Vec<(EndpointId, [u8; 32])>> {
        self.graph()
            .disjoint_paths(src, dst, max_paths)
            .into_iter()
            .filter_map(|path| {
                path.hops
                    .iter()
                    .map(|hop| self.seal_key(*hop).map(|key| (*hop, key)))
                    .collect::<Option<Vec<_>>>()
            })
            .collect()
    }

    /// A JSON-serializable snapshot of the routing topology from this node's
    /// point of view — the metric-labelled edges assembled from every held
    /// link-vector, plus which node is "us". Backs the `topology` IPC query and
    /// the `/square:topology` render.
    #[must_use]
    pub fn topology(&self, self_id: EndpointId) -> Topology {
        let edges = self
            .vectors
            .values()
            .flat_map(|vector| {
                vector.links.iter().map(move |(to, metric)| TopologyEdge {
                    from: vector.origin.to_string(),
                    to: to.to_string(),
                    metric: metric.0,
                })
            })
            .collect();
        Topology {
            self_id: self_id.to_string(),
            edges,
        }
    }

    /// The directed, metric-weighted graph assembled from every held vector.
    pub(crate) fn graph(&self) -> Graph {
        let mut graph = Graph::default();
        for vector in self.vectors.values() {
            for (neighbor, metric) in &vector.links {
                graph.insert_link(vector.origin, *neighbor, *metric);
            }
        }
        graph
    }
}

#[cfg(test)]
mod tests {
    use iroh::{EndpointId, SecretKey};

    use super::{LinkMetric, LinkStateStore, LinkVector};

    fn eid(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    fn vector(origin: EndpointId, seq: u64, links: &[(EndpointId, u32)]) -> LinkVector {
        LinkVector {
            origin,
            seq,
            seal_key: [7u8; 32],
            links: links
                .iter()
                .map(|(to, cost)| (*to, LinkMetric(*cost)))
                .collect(),
        }
    }

    #[test]
    fn newer_vector_supersedes_older() {
        let (na, nb) = (eid(1), eid(2));
        let mut store = LinkStateStore::default();
        assert!(store.ingest(vector(na, 1, &[(nb, 10)])));
        // Stale (lower seq) is rejected.
        assert!(!store.ingest(vector(na, 0, &[(nb, 1)])));
        // Newer wins.
        assert!(store.ingest(vector(na, 2, &[(nb, 1)])));
    }

    #[test]
    fn assembles_multiple_origins_into_one_graph() {
        let (na, nb, nc) = (eid(1), eid(2), eid(3));
        let mut store = LinkStateStore::default();
        store.ingest(vector(na, 1, &[(nb, 5)]));
        store.ingest(vector(nb, 1, &[(nc, 5)]));
        let graph = store.graph();
        let path = graph.shortest_path(na, nc).expect("assembled path");
        assert_eq!(path.hops, vec![nb, nc]);
        assert_eq!(path.cost, LinkMetric(10));
    }

    #[test]
    fn circuit_paths_attach_each_hop_key_or_drop_a_keyless_path() {
        let (na, nb, nc) = (eid(1), eid(2), eid(3));
        let mut store = LinkStateStore::default();
        // na→nb→nc, and both nb, nc advertise their keys (via `vector`).
        store.ingest(vector(na, 1, &[(nb, 5)]));
        store.ingest(vector(nb, 1, &[(nc, 5)]));
        store.ingest(vector(nc, 1, &[]));
        let paths = store.circuit_paths(na, nc, 3);
        assert_eq!(paths.len(), 1, "one path na→nb→nc");
        assert_eq!(
            paths[0].iter().map(|(hop, _)| *hop).collect::<Vec<_>>(),
            vec![nb, nc]
        );
        assert!(paths[0].iter().all(|(_, key)| *key == [7u8; 32]));

        // Drop nc's vector: the path exists in the graph but nc's key is unknown,
        // so it is filtered out and no usable circuit remains.
        let mut store2 = LinkStateStore::default();
        store2.ingest(vector(na, 1, &[(nb, 5)]));
        store2.ingest(vector(nb, 1, &[(nc, 5)]));
        assert!(
            store2.circuit_paths(na, nc, 3).is_empty(),
            "missing hop key ⇒ no usable circuit"
        );
    }

    #[test]
    fn circuit_paths_traces_the_four_node_chain() {
        // a→b→c→d chain, no shortcut: the sole circuit is the whole chain.
        let (na, nb, nc, nd) = (eid(1), eid(2), eid(3), eid(4));
        let mut store = LinkStateStore::default();
        store.ingest(vector(na, 1, &[(nb, 1)]));
        store.ingest(vector(nb, 1, &[(nc, 1)]));
        store.ingest(vector(nc, 1, &[(nd, 1)]));
        store.ingest(vector(nd, 1, &[]));
        let paths = store.circuit_paths(na, nd, 3);
        assert_eq!(
            paths[0].iter().map(|(hop, _)| *hop).collect::<Vec<_>>(),
            vec![nb, nc, nd]
        );
    }

    #[test]
    fn circuit_paths_picks_the_cheaper_direct_over_the_chain() {
        // Chain a→b→c→d (total 3) plus a cheaper direct a→d (cost 1): the shortcut
        // is chosen. A direct route has no interior hops, so the disjoint search
        // terminates on it — the chain is *not* returned as an alternate (a one-hop
        // path can't be made node-disjoint from anything).
        let (na, nb, nc, nd) = (eid(1), eid(2), eid(3), eid(4));
        let mut store = LinkStateStore::default();
        store.ingest(vector(na, 1, &[(nb, 1), (nd, 1)]));
        store.ingest(vector(nb, 1, &[(nc, 1)]));
        store.ingest(vector(nc, 1, &[(nd, 1)]));
        store.ingest(vector(nd, 1, &[]));
        let paths = store.circuit_paths(na, nd, 3);
        assert_eq!(
            paths.len(),
            1,
            "the interior-less direct route ends the search"
        );
        assert_eq!(
            paths[0].iter().map(|(hop, _)| *hop).collect::<Vec<_>>(),
            vec![nd],
            "the cheaper direct route is chosen"
        );
    }

    #[test]
    fn circuit_paths_returns_disjoint_alternates() {
        // Diamond na→{nb,nc}→nd: two node-disjoint circuits for failover.
        let (na, nb, nc, nd) = (eid(1), eid(2), eid(3), eid(4));
        let mut store = LinkStateStore::default();
        store.ingest(vector(na, 1, &[(nb, 1), (nc, 1)]));
        store.ingest(vector(nb, 1, &[(nd, 1)]));
        store.ingest(vector(nc, 1, &[(nd, 1)]));
        store.ingest(vector(nd, 1, &[]));
        let paths = store.circuit_paths(na, nd, 3);
        assert_eq!(paths.len(), 2, "both diamond arms");
        assert_ne!(paths[0][0].0, paths[1][0].0, "disjoint first hops");
    }

    #[test]
    fn removing_an_origin_drops_its_links() {
        let (na, nb, nc) = (eid(1), eid(2), eid(3));
        let mut store = LinkStateStore::default();
        store.ingest(vector(na, 1, &[(nb, 5)]));
        store.ingest(vector(nb, 1, &[(nc, 5)]));
        assert!(store.remove(nb));
        // With nb's links gone, nc is no longer reachable from na.
        assert!(store.graph().shortest_path(na, nc).is_none());
        assert!(!store.remove(nb), "second remove is a no-op");
    }
}
