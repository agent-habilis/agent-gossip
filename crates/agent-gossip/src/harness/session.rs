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
    pub async fn inject_raw(&self, bytes: bytes::Bytes) -> Result<(), crate::api::SessionError> {
        self.core.inject_raw(bytes).await
    }

    /// Simulate the gossip stream terminally ending (the daemon must
    /// resubscribe and recover on its own). Adversarial-suite only.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub async fn sever_gossip(&self) -> Result<(), crate::api::SessionError> {
        self.core.sever_gossip().await
    }

    /// Put the document of `channel` in `epoch` at once, as an adoption does after its wait: an
    /// honest node in a far epoch without 10 minutes of ticks. Adversarial-suite only.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub async fn force_epoch(
        &self,
        channel: habilis_network::protocol::Channel,
        epoch: u64,
    ) -> Result<(), crate::api::SessionError> {
        self.core.force_epoch(channel, epoch).await
    }

    /// Snapshot the reassembly store's accounting
    /// `(groups, total_bytes, max_author_bytes)`. Adversarial-suite only —
    /// lets it assert crafted shard streams stay inside the byte budgets.
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub async fn reassembly_stats(
        &self,
    ) -> Result<(usize, usize, usize), crate::api::SessionError> {
        self.core.reassembly_stats().await
    }
}
