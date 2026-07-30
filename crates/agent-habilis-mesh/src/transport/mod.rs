//! Everything that moves bytes for the mesh, on either plane:
//!
//! - [`deliver`] — the single cross-plane send decision: a broadcast rides
//!   gossip, a directed message goes over the **unicast** point-to-point QUIC
//!   channel only (`send`).
//! - The unicast plane machinery: the per-peer connection pool (`pool`) and
//!   the inbound acceptor (`accept`). Distinct from `link` (a gossip
//!   active-view neighbor) and from `Reach::Direct` (a gossip-overlay
//!   concept): a unicast connection is a real client/server QUIC link this
//!   node opens to one peer's endpoint, on its own ALPN, off the
//!   gossip flood.
//! - [`MeshSender`] — the gossip broadcast handle (`sender`).
//! - `ipc` — the unix-socket / named-pipe listener used by the CLI's `msg`
//!   and `poll` subcommands to talk to a running `create` or `join` daemon.
//!   (The MCP stdio server is a separate, consumer-side path.)
//!
//! Inbound unicast frames are funnelled into the *same* validation+dispatch
//! path as gossip (`gossip::ingest`), so signature-verify, the mesh gate, and
//! cross-transport dedup are identical on both planes.

mod accept;
pub(crate) mod ipc;
mod pool;
mod send;
pub(crate) mod sender;

pub(crate) use accept::UnicastAcceptor;
pub(crate) use pool::UnicastPool;
pub use send::Lane;
pub use send::deliver;
pub(crate) use send::lane_for;
pub use sender::MeshSender;

/// ALPN for the unicast channel — a raw bidirectional QUIC stream with its own
/// protocol identity, distinct from `GOSSIP_ALPN` and the application's own bridge ALPN.
/// Wire-load-bearing: changing it is a protocol break, so it moves only with
/// `message::VERSION` (it last changed when the byte-domains dropped the
/// product's name for the engine's).
pub(crate) const UNICAST_ALPN: &[u8] = b"habilis-mesh/unicast/1";

/// `tracing` target for the unicast plane (matches the module path so
/// `EnvFilter` prefix-matching works, e.g. `RUST_LOG=agent_habilis_mesh::transport=debug`).
pub(crate) const LOG_TARGET: &str = "agent_habilis_mesh::transport";
