//! The **unicast** transport plane: a point-to-point QUIC channel a
//! single-addressee message takes when its addressee is dialable, with gossip
//! as broadcast + fallback. Distinct from `link` (a gossip active-view
//! neighbor) and from `Reach::Direct` (which stays a gossip-overlay concept):
//! a unicast connection is a real client/server QUIC link this node opens to
//! one participant's endpoint, on its own ALPN, off the gossip flood.
//!
//! [`deliver`] is the single send decision — a message with exactly one
//! addressee we can reach over unicast goes p2p; everything else (a broadcast,
//! or an addressee we can't dial) goes over gossip. Inbound unicast frames are
//! funnelled into the *same* validation+dispatch path as gossip
//! (`gossip::ingest`), so signature-verify, swarm-gate, and cross-transport
//! dedup are identical on both planes.

mod accept;
mod pool;
mod send;

pub(crate) use accept::UnicastAcceptor;
pub(crate) use pool::UnicastPool;
pub(crate) use send::{Lane, deliver, lane_for};


/// ALPN for the unicast channel — a raw bidirectional QUIC stream with its own
/// protocol identity, distinct from `GOSSIP_ALPN` and the a2a bridge's ALPN.
pub(crate) const UNICAST_ALPN: &[u8] = b"agent-gossip/unicast/1";

/// `tracing` target for the unicast plane (matches the module path so
/// `EnvFilter` prefix-matching works, e.g. `RUST_LOG=agent_gossip::unicast=debug`).
pub(crate) const LOG_TARGET: &str = "agent_gossip::unicast";
