//! The **whisper** transport: multi-hop directed routing over the existing mesh.
//!
//! A directed message whose addressee we cannot reach with a *direct* unicast
//! dial is carried by a source-routed circuit through peers we are already
//! connected to. This module owns the routing brain — the metric-weighted mesh
//! graph assembled from peers' gossiped link-vectors, and the source-route
//! computation over it (shortest path + node-disjoint backups). The circuit
//! data plane (telescoping QUIC splice) and the send-path integration land in
//! later phases.

mod accept;
mod cell;
mod circuit;
mod graph;
mod linkstate;
mod metric;
mod telemetry;
mod wire;

pub(crate) use accept::WhisperAcceptor;
pub(crate) use circuit::open_circuit;
pub(crate) use linkstate::{LinkStateStore, LinkVector, self_vector};
pub(crate) use telemetry::NeighborProfile;

/// ALPN for the whisper circuit channel — a dedicated QUIC protocol, distinct from
/// `GOSSIP_ALPN` / `UNICAST_ALPN`, over which telescoping circuits are built and
/// spliced.
pub(crate) const WHISPER_ALPN: &[u8] = b"agent-gossip/whisper/1";

/// `tracing` target for the whisper plane (matches the module path so `EnvFilter`
/// prefix-matching works, e.g. `RUST_LOG=agent_gossip::whisper=debug`).
pub(crate) const LOG_TARGET: &str = "agent_gossip::whisper";
