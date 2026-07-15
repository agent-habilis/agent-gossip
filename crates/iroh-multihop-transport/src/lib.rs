//! A source-routed, multi-hop iroh **custom transport**.
//!
//! Registered on an iroh endpoint via the `unstable-custom-transports` seam,
//! this transport lets a peer reach a destination it cannot dial directly by
//! relaying the destination's QUIC packets through intermediate peers. iroh runs
//! its full QUIC state machine end-to-end, so the two endpoints share a real
//! [`iroh::endpoint::Connection`] (streams, congestion control, QUIC-TLS
//! end-to-end secrecy); the relays only forward opaque, already-encrypted
//! packets.
//!
//! # Shape
//!
//! - [`Topology`] is the routing brain: a metric-weighted link-state graph fed by
//!   peers' [`LinkVector`]s, producing node-disjoint source [`Route`]s.
//! - A computed [`Route`] is packed into a [`CustomAddr`](iroh_base::CustomAddr)
//!   and travels with the connection; the address lookup resolves a target
//!   endpoint id to one.
//! - [`MultihopHandle`] owns a dedicated **underlay** iroh endpoint that carries
//!   packets hop-by-hop, and wires the transport onto an application endpoint via
//!   its [`Preset`](iroh::endpoint::presets::Preset) impl.
//!
//! # Usage
//!
//! ```ignore
//! // A dedicated underlay endpoint the crate forwards over.
//! let underlay = Endpoint::builder(presets::N0).bind().await?;
//! let handle = MultihopHandle::new(app_secret.public(), underlay);
//! // Wire the transport, its address lookup, and the backup path selector.
//! let app = Endpoint::builder(presets::N0)
//!     .secret_key(app_secret)
//!     .preset(handle.clone())
//!     .bind()
//!     .await?;
//! // Feed live link-state so routes can be computed.
//! handle.feed_topology(link_vector);
//! ```

mod addr;
mod graph;
mod lookup;
mod metric;
mod preset;
mod selector;
mod topology;
mod transport;
mod underlay;
mod wire;

use std::sync::{Arc, RwLock};

use iroh::address_lookup::AddressLookup;
use iroh::endpoint::transports::{CustomTransport, PathSelector};
use iroh::{Endpoint, EndpointAddr, EndpointId};

pub use addr::{Route, RouteHop};
pub use metric::LinkMetric;
pub use topology::{LinkVector, Topology, TopologyEdge, TopologyView};

use crate::addr::Route as RouteInner;
use crate::lookup::MultihopLookup;
use crate::selector::MultihopBackup;
use crate::transport::{MultihopTransport, Shared};
use crate::underlay::{ForwardAcceptor, Forwarder};

/// Transport-type id for the multihop transport, tagging every multihop
/// [`CustomAddr`](iroh_base::CustomAddr) (see iroh's `TRANSPORTS.md` registry).
pub const MULTIHOP_TRANSPORT_ID: u64 = 0x6d68; // "mh"

/// Default number of node-disjoint routes the address lookup considers.
const DEFAULT_MAX_PATHS: usize = 3;

/// Terminal-delivery queue depth into the local transport's `poll_recv`.
const INBOUND_CAP: usize = 256;

/// A handle to a live multihop transport: its underlay endpoint, forwarder, and
/// routing table. Clone-cheap (`Arc` inside). Register it on an application
/// endpoint with [`Endpoint::builder(..).preset(handle)`](iroh::endpoint::presets::Preset),
/// then keep it to [`feed_topology`](Self::feed_topology) as link-state changes.
///
/// The `app_id` passed to [`new`](Self::new) **must** match the public key of the
/// application endpoint this handle is presetted onto — it is stamped as our hop
/// identity in every cell we originate.
#[derive(Clone, Debug)]
pub struct MultihopHandle {
    inner: Arc<HandleInner>,
}

#[derive(Debug)]
struct HandleInner {
    topology: Arc<RwLock<Topology>>,
    self_id: EndpointId,
    underlay: Endpoint,
    transport: Arc<MultihopTransport>,
    max_paths: usize,
    // Kept alive so the underlay's `FORWARD_ALPN` accept loop keeps running.
    _router: iroh::protocol::Router,
}

impl MultihopHandle {
    /// Build a handle over `underlay`, an endpoint dedicated to hop-by-hop
    /// forwarding. Must be called from within a tokio runtime (it spawns the
    /// forwarder's accept loop). `app_id` is the application endpoint's id.
    #[must_use]
    pub fn new(app_id: EndpointId, underlay: Endpoint) -> Self {
        Self::with_max_paths(app_id, underlay, DEFAULT_MAX_PATHS)
    }

    /// [`new`](Self::new) with an explicit disjoint-route ceiling.
    #[must_use]
    pub fn with_max_paths(app_id: EndpointId, underlay: Endpoint, max_paths: usize) -> Self {
        let (inbound_tx, inbound_rx) = tokio::sync::mpsc::channel(INBOUND_CAP);
        let forwarder = Arc::new(Forwarder::new(underlay.clone(), inbound_tx));

        let self_hop = RouteHop {
            app_id,
            underlay: underlay.addr(),
        };
        let self_addr = RouteInner::singleton(self_hop.clone()).encode();
        let shared = Arc::new(Shared {
            self_hop,
            self_addr,
            forwarder: Arc::clone(&forwarder),
        });

        let transport = Arc::new(MultihopTransport::new(shared, inbound_rx));
        let router = iroh::protocol::Router::builder(underlay.clone())
            .accept(underlay::FORWARD_ALPN, ForwardAcceptor::new(forwarder))
            .spawn();

        Self {
            inner: Arc::new(HandleInner {
                topology: Arc::new(RwLock::new(Topology::default())),
                self_id: app_id,
                underlay,
                transport,
                max_paths,
                _router: router,
            }),
        }
    }

    /// Ingest a peer's link-vector into the routing table. Returns whether the
    /// table changed (i.e. the vector was newer than what we held).
    ///
    /// # Panics
    /// If the routing-table lock is poisoned by a panic in another thread.
    #[expect(
        clippy::must_use_candidate,
        reason = "the returned 'changed' flag is advisory; ignoring it is valid"
    )]
    pub fn feed_topology(&self, vector: LinkVector) -> bool {
        self.inner
            .topology
            .write()
            .expect("topology lock poisoned")
            .ingest(vector)
    }

    /// Drop an origin's advertised links (e.g. a peer that left the mesh).
    ///
    /// # Panics
    /// If the routing-table lock is poisoned by a panic in another thread.
    #[expect(
        clippy::must_use_candidate,
        reason = "the returned 'removed' flag is advisory; ignoring it is valid"
    )]
    pub fn remove_origin(&self, origin: EndpointId) -> bool {
        self.inner
            .topology
            .write()
            .expect("topology lock poisoned")
            .remove(origin)
    }

    /// This node's current underlay dial address, for advertising to peers so
    /// they can route through us.
    #[must_use]
    pub fn underlay_addr(&self) -> EndpointAddr {
        self.inner.underlay.addr()
    }

    /// Build this node's own link-vector for gossiping: our underlay address plus
    /// one `(neighbour, cost)` link per direct neighbour.
    #[must_use]
    pub fn link_vector(&self, seq: u64, links: Vec<(EndpointId, u32)>) -> LinkVector {
        LinkVector::new(self.inner.self_id, seq, self.underlay_addr(), links)
    }

    /// A JSON-serializable snapshot of the routing graph from this node's point of
    /// view, for the `topology` IPC query / `/square:topology` render.
    ///
    /// # Panics
    /// If the routing-table lock is poisoned by a panic in another thread.
    #[must_use]
    pub fn topology_view(&self) -> TopologyView {
        self.inner
            .topology
            .read()
            .expect("topology lock poisoned")
            .view(self.inner.self_id)
    }

    /// The custom transport factory, for `Builder::add_custom_transport`.
    #[must_use]
    pub fn custom_transport(&self) -> Arc<dyn CustomTransport> {
        Arc::clone(&self.inner.transport) as Arc<dyn CustomTransport>
    }

    /// The address lookup that resolves a target endpoint id to a routed multihop
    /// address, for `Builder::address_lookup`.
    #[must_use]
    pub fn address_lookup(&self) -> impl AddressLookup + use<> {
        MultihopLookup::new(
            self.inner.self_id,
            Arc::clone(&self.inner.topology),
            self.inner.max_paths,
        )
    }

    /// The backup path selector (multihop loses to any direct/relay path), for
    /// `Builder::path_selector`.
    #[must_use]
    pub fn path_selector(&self) -> Arc<dyn PathSelector> {
        Arc::new(MultihopBackup)
    }
}
