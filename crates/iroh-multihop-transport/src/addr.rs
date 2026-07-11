//! The route carried inside a multihop [`CustomAddr`].
//!
//! A multihop address is not a location — it is a **source route**: the ordered
//! hops from the sender to the destination, each carrying the underlay
//! [`EndpointAddr`] needed to dial it. iroh treats the whole encoded route as an
//! opaque path locator; we pack/unpack it here.
//!
//! Reachability-first: the route is *reversible*. A terminal derives the return
//! route from the forward route plus the sender's own hop, so a reply needs no
//! fresh lookup (see [`Route::reverse_from`]). This assumes the underlay links
//! are usable in both directions, which the bidirectional link-state graph
//! already models.

use iroh::{EndpointAddr, EndpointId};
use iroh_base::CustomAddr;
use serde::{Deserialize, Serialize};

use crate::MULTIHOP_TRANSPORT_ID;

/// One hop in a source route: which node it is (its app-layer [`EndpointId`])
/// and how to dial its multihop **underlay** endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteHop {
    pub(crate) app_id: EndpointId,
    pub(crate) underlay: EndpointAddr,
}

/// A source route: the ordered hops **after** the sender, ending at the
/// destination. Empty is invalid (there is nothing to send to).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route(pub(crate) Vec<RouteHop>);

impl Route {
    pub(crate) fn hops(&self) -> &[RouteHop] {
        &self.0
    }

    /// Encode into the opaque `data` of a multihop [`CustomAddr`]. postcard is
    /// deterministic, so the same route always yields byte-identical addresses —
    /// which iroh relies on to dedupe a peer's path.
    pub(crate) fn encode(&self) -> CustomAddr {
        let bytes = postcard::to_allocvec(self).expect("route serializes");
        CustomAddr::from_parts(MULTIHOP_TRANSPORT_ID, &bytes)
    }

    /// Decode a multihop [`CustomAddr`] back into a route. Returns `None` for a
    /// wrong transport id or malformed bytes.
    pub(crate) fn decode(addr: &CustomAddr) -> Option<Self> {
        if addr.id() != MULTIHOP_TRANSPORT_ID {
            return None;
        }
        postcard::from_bytes(addr.data()).ok()
    }

    /// The return route a terminal should hand back as its remote address, given
    /// the forward route it received and the original `source` hop.
    ///
    /// Forward `S → [R1, R2, B]` (source `S`) reverses to `B → [R2, R1, S]`:
    /// drop the destination (`B`, which is us), reverse the interior, then append
    /// the source. Applying this again at `S` reproduces the original forward
    /// route, so the two ends agree on one stable path locator each.
    pub(crate) fn reverse_from(forward: &[RouteHop], source: RouteHop) -> Self {
        let mut hops: Vec<RouteHop> = forward
            .iter()
            .rev()
            .skip(1) // drop the destination (ourselves)
            .cloned()
            .collect();
        hops.push(source);
        Route(hops)
    }
}

#[cfg(test)]
mod tests {
    use super::{Route, RouteHop};
    use iroh::{EndpointAddr, EndpointId, SecretKey};

    fn hop(seed: u8) -> RouteHop {
        let id: EndpointId = SecretKey::from_bytes(&[seed; 32]).public();
        RouteHop {
            app_id: id,
            underlay: EndpointAddr::new(id),
        }
    }

    #[test]
    fn encode_decode_roundtrips() {
        let route = Route(vec![hop(1), hop(2), hop(3)]);
        let addr = route.encode();
        assert_eq!(Route::decode(&addr).expect("decodes"), route);
    }

    #[test]
    fn wrong_transport_id_does_not_decode() {
        let route = Route(vec![hop(1)]);
        let addr = route.encode();
        let foreign = iroh_base::CustomAddr::from_parts(0x99, addr.data());
        assert!(Route::decode(&foreign).is_none());
    }

    #[test]
    fn reverse_is_an_involution_across_the_two_ends() {
        // Source S=hop(9); forward route to B: [R1, R2, B].
        let (source, r1, r2, dest) = (hop(9), hop(1), hop(2), hop(3));
        let forward = vec![r1.clone(), r2.clone(), dest.clone()];

        // At B, the return route is [R2, R1, S].
        let ret = Route::reverse_from(&forward, source.clone());
        assert_eq!(ret.0, vec![r2, r1, source]);

        // Reversing again at S (source now B) reproduces the forward route.
        let back = Route::reverse_from(&ret.0, dest);
        assert_eq!(back.0, forward);
    }
}
