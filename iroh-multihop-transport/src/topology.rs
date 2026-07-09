//! Link-state dissemination: a peer's advertised **link-vector** (the links it
//! measures itself, plus its own underlay dial address) and the assembly of many
//! peers' vectors into the routing [`Graph`] and concrete source [`Route`]s.
//!
//! Each origin advertises only its *own* outbound links and its own underlay
//! address, carrying a monotonic `seq` so a newer vector supersedes an older one
//! (last-writer-wins per origin). Every node folds the freshest vector from each
//! origin into one metric-weighted graph and runs Dijkstra locally.

use std::collections::HashMap;

use iroh::{EndpointAddr, EndpointId};
use serde::{Deserialize, Serialize};

use crate::addr::{Route, RouteHop};
use crate::graph::Graph;
use crate::metric::LinkMetric;

/// One peer's advertised view of its **own** outbound links plus how to dial its
/// multihop underlay — the gossiped payload. `seq` orders successive vectors
/// from the same `origin`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkVector {
    pub(crate) origin: EndpointId,
    pub(crate) seq: u64,
    /// How to reach this origin's multihop **underlay** endpoint. A hop with no
    /// advertised underlay address cannot be dialed, so any route through it is
    /// dropped.
    pub(crate) underlay: EndpointAddr,
    pub(crate) links: Vec<(EndpointId, LinkMetric)>,
}

impl LinkVector {
    /// Build a vector from this node's direct neighbours and their measured
    /// costs. `underlay` is our own underlay [`EndpointAddr`], advertised so
    /// peers can route through us.
    #[must_use]
    pub fn new(
        origin: EndpointId,
        seq: u64,
        underlay: EndpointAddr,
        links: Vec<(EndpointId, u32)>,
    ) -> Self {
        Self {
            origin,
            seq,
            underlay,
            links: links
                .into_iter()
                .map(|(id, cost)| (id, LinkMetric(cost)))
                .collect(),
        }
    }
}

/// The received link-vectors, kept freshest-per-origin — the routing table's raw
/// material. [`route_to`](Topology::route_to) folds them into the graph on demand.
#[derive(Debug, Default)]
pub struct Topology {
    vectors: HashMap<EndpointId, LinkVector>,
}

impl Topology {
    /// Ingest a received vector, keeping it only if it is newer (`seq`) than what
    /// we already hold for that origin. Returns whether the store changed.
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
    pub fn remove(&mut self, origin: EndpointId) -> bool {
        self.vectors.remove(&origin).is_some()
    }

    /// The underlay address an origin advertised, for dialing a hop through it.
    fn underlay(&self, origin: EndpointId) -> Option<EndpointAddr> {
        self.vectors.get(&origin).map(|vector| vector.underlay.clone())
    }

    /// Up to `max_paths` **node-disjoint** source [`Route`]s from `src` to `dst`,
    /// shortest first — the primary plus failover candidates. A route is dropped
    /// if any hop hasn't advertised its underlay address yet (we can't dial a hop
    /// we can't reach), so the returned list may be shorter than `max_paths` or
    /// empty.
    #[must_use]
    pub fn route_to(&self, src: EndpointId, dst: EndpointId, max_paths: usize) -> Vec<Route> {
        self.graph()
            .disjoint_paths(src, dst, max_paths)
            .into_iter()
            .filter_map(|path| {
                let hops = path
                    .hops
                    .iter()
                    .map(|hop| {
                        self.underlay(*hop).map(|underlay| RouteHop {
                            app_id: *hop,
                            underlay,
                        })
                    })
                    .collect::<Option<Vec<_>>>()?;
                Some(Route(hops))
            })
            .collect()
    }

    /// A JSON-serializable snapshot of the routing graph from `self_id`'s point
    /// of view — every metric-labelled edge assembled from the held vectors, plus
    /// which node is "us". Backs the `topology` IPC query / `/square:topology`.
    #[must_use]
    pub fn view(&self, self_id: EndpointId) -> TopologyView {
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
        TopologyView {
            self_id: self_id.to_string(),
            edges,
        }
    }

    /// The directed, metric-weighted graph assembled from every held vector.
    fn graph(&self) -> Graph {
        let mut graph = Graph::default();
        for vector in self.vectors.values() {
            for (neighbor, metric) in &vector.links {
                graph.insert_link(vector.origin, *neighbor, *metric);
            }
        }
        graph
    }
}

/// A node's-eye view of the routing graph, ready to serialize to JSON for the
/// `topology` IPC query and the `/square:topology` render.
#[derive(Debug, serde::Serialize)]
pub struct TopologyView {
    /// This node's own endpoint id (hex) — the graph's "you".
    pub self_id: String,
    pub edges: Vec<TopologyEdge>,
}

/// One directed, metric-labelled edge in a [`TopologyView`].
#[derive(Debug, serde::Serialize)]
pub struct TopologyEdge {
    pub from: String,
    pub to: String,
    pub metric: u32,
}

#[cfg(test)]
mod tests {
    use super::{LinkVector, Topology};
    use iroh::{EndpointAddr, EndpointId, SecretKey};

    fn eid(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    fn vector(origin: EndpointId, seq: u64, links: &[(EndpointId, u32)]) -> LinkVector {
        LinkVector::new(
            origin,
            seq,
            EndpointAddr::new(origin),
            links.iter().map(|(to, cost)| (*to, *cost)).collect(),
        )
    }

    #[test]
    fn newer_vector_supersedes_older() {
        let (na, nb) = (eid(1), eid(2));
        let mut store = Topology::default();
        assert!(store.ingest(vector(na, 1, &[(nb, 10)])));
        assert!(!store.ingest(vector(na, 0, &[(nb, 1)])));
        assert!(store.ingest(vector(na, 2, &[(nb, 1)])));
    }

    #[test]
    fn route_traces_the_chain_and_carries_underlay_addrs() {
        let (na, nb, nc, nd) = (eid(1), eid(2), eid(3), eid(4));
        let mut store = Topology::default();
        store.ingest(vector(na, 1, &[(nb, 1)]));
        store.ingest(vector(nb, 1, &[(nc, 1)]));
        store.ingest(vector(nc, 1, &[(nd, 1)]));
        store.ingest(vector(nd, 1, &[]));
        let routes = store.route_to(na, nd, 3);
        assert_eq!(routes.len(), 1);
        assert_eq!(
            routes[0].hops().iter().map(|hop| hop.app_id).collect::<Vec<_>>(),
            vec![nb, nc, nd]
        );
        // Every hop carries a dialable underlay addr for that node.
        assert_eq!(routes[0].hops()[0].underlay, EndpointAddr::new(nb));
    }

    #[test]
    fn a_hop_without_an_underlay_addr_drops_the_route() {
        let (na, nb, nc) = (eid(1), eid(2), eid(3));
        let mut store = Topology::default();
        store.ingest(vector(na, 1, &[(nb, 1)]));
        store.ingest(vector(nb, 1, &[(nc, 1)]));
        // nc never advertised its own vector, so its underlay addr is unknown.
        assert!(
            store.route_to(na, nc, 3).is_empty(),
            "missing hop underlay ⇒ no usable route"
        );
    }

    #[test]
    fn prefers_the_cheaper_direct_over_the_chain() {
        let (na, nb, nc, nd) = (eid(1), eid(2), eid(3), eid(4));
        let mut store = Topology::default();
        store.ingest(vector(na, 1, &[(nb, 1), (nd, 1)]));
        store.ingest(vector(nb, 1, &[(nc, 1)]));
        store.ingest(vector(nc, 1, &[(nd, 1)]));
        store.ingest(vector(nd, 1, &[]));
        let routes = store.route_to(na, nd, 3);
        assert_eq!(routes.len(), 1, "the interior-less direct route ends the search");
        assert_eq!(
            routes[0].hops().iter().map(|hop| hop.app_id).collect::<Vec<_>>(),
            vec![nd]
        );
    }

    #[test]
    fn returns_disjoint_alternates() {
        let (na, nb, nc, nd) = (eid(1), eid(2), eid(3), eid(4));
        let mut store = Topology::default();
        store.ingest(vector(na, 1, &[(nb, 1), (nc, 1)]));
        store.ingest(vector(nb, 1, &[(nd, 1)]));
        store.ingest(vector(nc, 1, &[(nd, 1)]));
        store.ingest(vector(nd, 1, &[]));
        let routes = store.route_to(na, nd, 3);
        assert_eq!(routes.len(), 2, "both diamond arms");
        assert_ne!(routes[0].hops()[0].app_id, routes[1].hops()[0].app_id);
    }
}
