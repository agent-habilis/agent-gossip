//! The public `MeshSession` testkit: the crafted-message injector and the
//! index/reassembly probes the adversarial suite drives.
//!
//! These live here, not on the API itself, so the `adversarial` feature cannot
//! widen the curated public surface — and so `iroh::EndpointId` never appears in
//! `api`, keeping its "no iroh type crosses this boundary" claim structural
//! rather than incidental. They stay *inherent* methods on `MeshSession`, so the
//! suite calls them exactly as before with no import.

use crate::api::MeshSession;

/// A synthetic link-state vector for [`MeshSession::inject_link_vector`]: one
/// peer's outbound edges, as if freshly gossiped. Lives here rather than in
/// `api` because it is the one testkit type that would put an `iroh` type on the
/// public surface.
#[derive(Debug)]
pub struct LinkVectorParams {
    /// The vector's origin (the peer whose edges these are).
    pub origin: iroh::EndpointId,
    /// Monotonic per-origin sequence — a higher value wins over what the
    /// mesh already converged on.
    pub seq: u64,
    /// The origin's `(neighbour, cost)` outbound edges.
    pub links: Vec<(iroh::EndpointId, u32)>,
}

impl MeshSession {
    /// Broadcast pre-built wire bytes **verbatim** into the mesh — no
    /// signing, no chain stamping. Test-only escape hatch (the `adversarial`
    /// feature) for injecting crafted/malicious messages a correct client
    /// would never produce, so the adversarial suite can prove receivers
    /// reject or flag them. Not part of the normal public API.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub async fn inject_raw(&self, bytes: Vec<u8>) -> anyhow::Result<()> {
        self.core.inject_raw(bytes::Bytes::from(bytes)).await
    }

    /// This node's iroh endpoint id. Test-only (`adversarial`): a peer needs it
    /// to name this node in an injected circuit topology.
    #[must_use]
    pub fn endpoint_id(&self) -> iroh::EndpointId {
        self.core.endpoint_id()
    }

    /// This node's X25519 circuit key. Test-only (`adversarial`): a peer needs
    /// it to onion-seal a circuit terminating at this node.
    #[must_use]
    pub fn circuit_key(&self) -> [u8; 32] {
        self.core.circuit_key()
    }

    /// Ingest a synthetic link-state vector into this node's multihop routing
    /// table. Test-only (`adversarial`). NOTE: with the datagram-based multihop
    /// transport a route only forwards over *live* underlay endpoints, so an
    /// injected vector no longer yields a forwarding route; real multi-hop
    /// delivery is covered by `iroh-multihop-transport`'s own e2e tests.
    /// `origin`/`links` are a peer's endpoint id and `(neighbour, cost)` edges.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub async fn inject_link_vector(&self, vector: LinkVectorParams) -> anyhow::Result<()> {
        let LinkVectorParams { origin, seq, links } = vector;
        self.core.inject_link_vector(origin, seq, links).await
    }

    /// Simulate the gossip stream terminally ending (the daemon must
    /// resubscribe and recover on its own). Adversarial-suite only.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub async fn sever_gossip(&self) -> anyhow::Result<()> {
        self.core.sever_gossip().await
    }

    /// Snapshot the fork/DAG index sizes `(by_hash, dag_heads, author_seqs)`.
    /// Adversarial-suite only — lets it assert that messages we don't
    /// retain are never folded into the indexes (no unbounded leak).
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub async fn index_stats(&self) -> anyhow::Result<(usize, usize, usize)> {
        self.core.index_stats().await
    }

    /// Snapshot the reassembly store's accounting
    /// `(groups, total_bytes, max_author_bytes)`. Adversarial-suite only —
    /// lets it assert crafted shard streams stay inside the byte budgets.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub async fn reassembly_stats(&self) -> anyhow::Result<(usize, usize, usize)> {
        self.core.reassembly_stats().await
    }
}
