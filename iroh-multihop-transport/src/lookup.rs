//! Resolving an endpoint id to a multihop [`CustomAddr`](iroh_base::CustomAddr).
//!
//! This is the initiator-side authority: given the local [`Topology`], compute
//! the best source route to a target and hand iroh the route packed into a
//! custom transport address. iroh then forms an end-to-end QUIC connection whose
//! packets ride that route. Because the route is self-contained, no hop needs a
//! separate lookup — the destination even derives its reply route from the cell.

use std::sync::{Arc, RwLock};

use iroh::EndpointId;
use iroh::TransportAddr;
use iroh::address_lookup::{AddressLookup, Error, Item};
use iroh::endpoint_info::{EndpointData, EndpointInfo};
use n0_future::stream::{self, Boxed};

use crate::topology::Topology;

/// Provenance tag iroh attaches to items this lookup produces.
const PROVENANCE: &str = "iroh-multihop";

#[derive(Debug)]
pub(crate) struct MultihopLookup {
    self_id: EndpointId,
    topology: Arc<RwLock<Topology>>,
    max_paths: usize,
}

impl MultihopLookup {
    pub(crate) fn new(
        self_id: EndpointId,
        topology: Arc<RwLock<Topology>>,
        max_paths: usize,
    ) -> Self {
        Self {
            self_id,
            topology,
            max_paths,
        }
    }
}

impl AddressLookup for MultihopLookup {
    fn publish(&self, _data: &EndpointData) {}

    fn resolve(&self, endpoint_id: EndpointId) -> Option<Boxed<Result<Item, Error>>> {
        // Best (lowest-cost) disjoint route; the pool's alternates are for the
        // send-path's own failover, not iroh's path set.
        let route = self
            .topology
            .read()
            .expect("topology lock poisoned")
            .route_to(self.self_id, endpoint_id, self.max_paths)
            .into_iter()
            .next()?;
        let info = EndpointInfo::from_parts(
            endpoint_id,
            EndpointData::from_iter([TransportAddr::Custom(route.encode())]),
        );
        Some(Box::pin(stream::once(Ok(Item::new(info, PROVENANCE, None)))))
    }
}
