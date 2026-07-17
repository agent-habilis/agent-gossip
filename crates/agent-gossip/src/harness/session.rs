//! The public `MeshSession` testkit: the crafted-message injector and the
//! index/reassembly probes the adversarial suite drives.
//!
//! These live here, not on the API itself, so the `adversarial` feature cannot
//! widen the curated public surface. They stay *inherent* methods on
//! `MeshSession`, so the suite calls them exactly as before with no import.

use crate::api::MeshSession;

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
