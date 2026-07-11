//! The link-state graph as a metric-weighted directed graph, assembled from
//! peers' gossiped link-vectors, and the source-route computation over it:
//! shortest path (Dijkstra) plus node-disjoint backups.
//!
//! The initiator holds this graph and picks the whole hop sequence itself —
//! source routing (each hop only knows its neighbours; only the initiator can
//! reason about a whole path).

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

use iroh::EndpointId;

use crate::metric::LinkMetric;

/// A directed, metric-weighted view of the link-state graph: for each node, its
/// outbound links `(neighbour, cost)`. Nodes are keyed by their stable `EndpointId`.
#[derive(Debug, Default, Clone)]
pub(crate) struct Graph {
    adj: HashMap<EndpointId, Vec<(EndpointId, LinkMetric)>>,
}

/// A computed source route: the ordered hops **after** the source, up to and
/// including the destination (`[first_hop, …, dst]`), and the path's total cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Path {
    pub(crate) hops: Vec<EndpointId>,
    pub(crate) cost: LinkMetric,
}

impl Graph {
    /// Record a usable directed link `from -> to`. An unusable
    /// ([`LinkMetric::INFINITE`]) cost is dropped — an unusable link is the same
    /// as an absent one.
    pub(crate) fn insert_link(&mut self, from: EndpointId, to: EndpointId, cost: LinkMetric) {
        if cost.is_usable() {
            self.adj.entry(from).or_default().push((to, cost));
        }
    }

    /// Shortest path from `src` to `dst` by total [`LinkMetric`], or `None` when
    /// `dst` is unreachable.
    #[cfg(test)]
    pub(crate) fn shortest_path(&self, src: EndpointId, dst: EndpointId) -> Option<Path> {
        self.shortest_path_avoiding(src, dst, &HashSet::new())
    }

    /// Up to `k` **node-disjoint** paths from `src` to `dst`, shortest first.
    /// Greedy: take the shortest, block its interior hops, repeat.
    /// Interior-disjoint so one dead hop can't sever two circuits at once
    /// (`src`/`dst` are shared by construction). Stops early when no further
    /// disjoint path exists.
    pub(crate) fn disjoint_paths(
        &self,
        src: EndpointId,
        dst: EndpointId,
        max_paths: usize,
    ) -> Vec<Path> {
        let mut paths = Vec::new();
        let mut blocked: HashSet<EndpointId> = HashSet::new();
        for _ in 0..max_paths {
            let Some(path) = self.shortest_path_avoiding(src, dst, &blocked) else {
                break;
            };
            // Interior hops = every hop except the final destination.
            let interior: Vec<EndpointId> = path.hops.iter().rev().skip(1).copied().collect();
            let exhausted = interior.is_empty();
            blocked.extend(interior);
            paths.push(path);
            if exhausted {
                // A direct (interior-less) path has no disjoint alternative.
                break;
            }
        }
        paths
    }

    /// Dijkstra that treats every node in `blocked` as absent — the primitive
    /// behind node-disjoint search. `dst` is never blocked even if the caller
    /// lists it.
    fn shortest_path_avoiding(
        &self,
        src: EndpointId,
        dst: EndpointId,
        blocked: &HashSet<EndpointId>,
    ) -> Option<Path> {
        let mut best: HashMap<EndpointId, LinkMetric> = HashMap::new();
        let mut prev: HashMap<EndpointId, EndpointId> = HashMap::new();
        let mut heap: BinaryHeap<HeapEntry> = BinaryHeap::new();

        best.insert(src, LinkMetric::ZERO);
        heap.push(HeapEntry {
            cost: LinkMetric::ZERO,
            node: src,
        });

        while let Some(HeapEntry { cost, node }) = heap.pop() {
            if node == dst {
                return Some(Path {
                    hops: reconstruct(&prev, src, dst),
                    cost,
                });
            }
            // Skip a stale heap entry superseded by a cheaper relaxation.
            if best.get(&node).is_some_and(|known| *known < cost) {
                continue;
            }
            for (next, edge) in self.adj.get(&node).into_iter().flatten() {
                let candidate = RelaxEdge {
                    node,
                    cost,
                    next: *next,
                    weight: *edge,
                    dst,
                    blocked,
                };
                relax(&mut best, &mut prev, &mut heap, candidate);
            }
        }
        None
    }
}

/// The edge under consideration by one relaxation step: `node -> next` weighing
/// `weight` atop the already-settled `cost` to `node`, gated by `dst` (never
/// itself blocked) and the `blocked` set.
#[derive(Clone, Copy)]
struct RelaxEdge<'a> {
    node: EndpointId,
    cost: LinkMetric,
    next: EndpointId,
    weight: LinkMetric,
    dst: EndpointId,
    blocked: &'a HashSet<EndpointId>,
}

/// Relax `candidate`: skip it when `next` is blocked (unless it's `dst` itself),
/// then update `best`/`prev`/`heap` if the new cost improves on any previously
/// known cost to `next`.
fn relax(
    best: &mut HashMap<EndpointId, LinkMetric>,
    prev: &mut HashMap<EndpointId, EndpointId>,
    heap: &mut BinaryHeap<HeapEntry>,
    candidate: RelaxEdge<'_>,
) {
    let RelaxEdge {
        node,
        cost,
        next,
        weight,
        dst,
        blocked,
    } = candidate;
    if next != dst && blocked.contains(&next) {
        return;
    }
    let next_cost = cost.add(weight);
    let improved = match best.get(&next) {
        Some(known) => next_cost < *known,
        None => true,
    };
    if !improved {
        return;
    }
    best.insert(next, next_cost);
    prev.insert(next, node);
    heap.push(HeapEntry {
        cost: next_cost,
        node: next,
    });
}

/// Walk `prev` from `dst` back to `src`, returning the hops after `src` in
/// forward order (`[first_hop, …, dst]`).
fn reconstruct(
    prev: &HashMap<EndpointId, EndpointId>,
    src: EndpointId,
    dst: EndpointId,
) -> Vec<EndpointId> {
    let mut hops = vec![dst];
    let mut cur = dst;
    while let Some(&parent) = prev.get(&cur) {
        if parent == src {
            break;
        }
        hops.push(parent);
        cur = parent;
    }
    hops.reverse();
    hops
}

/// A min-heap entry ordered by cost only, so the graph never needs `Ord` on
/// `EndpointId`. The `Ord` impl is reversed so `BinaryHeap` (a max-heap) yields
/// the lowest-cost node first.
struct HeapEntry {
    cost: LinkMetric,
    node: EndpointId,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.cost == other.cost
    }
}

impl Eq for HeapEntry {}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed: the smallest cost is the greatest heap element.
        other.cost.cmp(&self.cost)
    }
}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use iroh::{EndpointId, SecretKey};

    use super::{Graph, LinkMetric};

    fn eid(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    /// A bidirectional link `left <-> right` of equal cost each way.
    fn link(graph: &mut Graph, left: EndpointId, right: EndpointId, cost: u32) {
        graph.insert_link(left, right, LinkMetric(cost));
        graph.insert_link(right, left, LinkMetric(cost));
    }

    #[test]
    fn shortest_path_traces_a_line() {
        let (na, nb, nc) = (eid(1), eid(2), eid(3));
        let mut graph = Graph::default();
        link(&mut graph, na, nb, 5);
        link(&mut graph, nb, nc, 7);
        let path = graph.shortest_path(na, nc).expect("reachable");
        assert_eq!(path.hops, vec![nb, nc]);
        assert_eq!(path.cost, LinkMetric(12));
    }

    #[test]
    fn shortest_path_prefers_a_cheaper_direct_over_the_chain() {
        let (na, nb, nc, nd) = (eid(1), eid(2), eid(3), eid(4));
        let mut graph = Graph::default();
        link(&mut graph, na, nb, 1);
        link(&mut graph, nb, nc, 1);
        link(&mut graph, nc, nd, 1);
        link(&mut graph, na, nd, 1);
        let path = graph.shortest_path(na, nd).expect("reachable");
        assert_eq!(path.hops, vec![nd]);
        assert_eq!(path.cost, LinkMetric(1));
    }

    #[test]
    fn shortest_path_prefers_the_chain_when_the_direct_link_is_costlier() {
        let (na, nb, nc, nd) = (eid(1), eid(2), eid(3), eid(4));
        let mut graph = Graph::default();
        link(&mut graph, na, nb, 1);
        link(&mut graph, nb, nc, 1);
        link(&mut graph, nc, nd, 1);
        link(&mut graph, na, nd, 10);
        let path = graph.shortest_path(na, nd).expect("reachable");
        assert_eq!(path.hops, vec![nb, nc, nd]);
        assert_eq!(path.cost, LinkMetric(3));
    }

    #[test]
    fn unreachable_destination_is_none() {
        let (na, nb, lonely) = (eid(1), eid(2), eid(9));
        let mut graph = Graph::default();
        link(&mut graph, na, nb, 1);
        assert!(graph.shortest_path(na, lonely).is_none());
    }

    #[test]
    fn infinite_links_are_dropped() {
        let (na, nb) = (eid(1), eid(2));
        let mut graph = Graph::default();
        graph.insert_link(na, nb, LinkMetric::INFINITE);
        assert!(graph.shortest_path(na, nb).is_none());
    }

    #[test]
    fn disjoint_paths_are_node_disjoint() {
        // Diamond: na -> {nb, nc} -> nd. Two interior-disjoint paths exist.
        let (na, nb, nc, nd) = (eid(1), eid(2), eid(3), eid(4));
        let mut graph = Graph::default();
        link(&mut graph, na, nb, 1);
        link(&mut graph, nb, nd, 1);
        link(&mut graph, na, nc, 1);
        link(&mut graph, nc, nd, 1);
        let paths = graph.disjoint_paths(na, nd, 2);
        assert_eq!(paths.len(), 2, "both diamond arms are available");
        assert_ne!(paths[0].hops[0], paths[1].hops[0]);
        for path in &paths {
            assert_eq!(*path.hops.last().unwrap(), nd);
        }
    }

    #[test]
    fn disjoint_paths_stops_when_no_alternative_exists() {
        let (na, nr, nd) = (eid(1), eid(5), eid(4));
        let mut graph = Graph::default();
        link(&mut graph, na, nr, 1);
        link(&mut graph, nr, nd, 1);
        let paths = graph.disjoint_paths(na, nd, 3);
        assert_eq!(
            paths.len(),
            1,
            "the lone hop can't be reused for a 2nd path"
        );
        assert_eq!(paths[0].hops, vec![nr, nd]);
    }
}
