use agent_habilis_mesh::directory::directory_mesh;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::task::JoinHandle;

use super::MeshSession;
use agent_habilis_mesh::daemon::EventLoopConfig;
use agent_habilis_mesh::directory;
use agent_habilis_mesh::protocol::mesh::{LookupOpts, MeshName};
use agent_habilis_mesh::util::tuning::advertise_interval_secs;

pub(crate) use agent_habilis_mesh::daemon::config::DIRECTORY_ADVERTISER_COHOST;

/// Spawn the directory re-broadcast task for `cfg`'s mesh: wire a fresh
/// live-peer counter into `cfg.live_count`, then re-send the
/// mesh's `💬…` id (with that count) into `directory` every
/// `ADVERTISE_INTERVAL_SECS` over the mesh's own `lookups`. Returns the
/// task handle so the owner can abort it (the inner directory session is
/// dropped with the task, closing that membership). A directory-join
/// failure logs and ends the task — the mesh is unaffected, just unlisted.
pub(crate) struct Advertiser(JoinHandle<()>);

impl fmt::Debug for Advertiser {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Advertiser").finish_non_exhaustive()
    }
}

// Aborts rather than detaches: a bare `JoinHandle` dropped on the floor keeps
// running, and we must stop re-broadcasting a mesh we have left. Owning the
// abort here is also what lets `InProcessSession` have no `Drop` of its own —
// which it cannot have, because `leave(self)` moves its `Node` out.
impl Drop for Advertiser {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(crate) fn spawn_advertiser(
    cfg: &mut EventLoopConfig,
    directory: MeshName,
    lookups: LookupOpts,
) -> Advertiser {
    let live_count = Arc::new(AtomicUsize::new(1));
    cfg.live_count = Some(live_count.clone());
    let mesh_id = cfg.mesh.clone();
    Advertiser(tokio::spawn(async move {
        // Reach the directory over the advertised mesh's own lookups, so an
        // mDNS-only mesh advertises over mDNS only (no DHT/relay touched) and
        // discoverers must use the same lookups to meet.
        let mesh = directory_mesh(&directory, lookups);
        // Co-host the directory rendezvous from t=0 so a beacon exists
        // before any discoverer subscribes (a `Deferred` advertiser would
        // only beacon at the first heal tick, after discoverers already
        // failed their first graft). Probe-first; see DIRECTORY_ADVERTISER_COHOST.
        let session = match MeshSession::join_decoded(mesh, None, DIRECTORY_ADVERTISER_COHOST).await
        {
            Ok(session) => session,
            Err(error) => {
                tracing::warn!(
                    target: "agent_habilis_mesh::directory",
                    %error,
                    directory = %directory,
                    "directory advertise: could not join the directory; gossip stays unlisted"
                );
                return;
            }
        };
        let mut ticker = tokio::time::interval(Duration::from_secs(advertise_interval_secs()));
        loop {
            ticker.tick().await;
            let ad = directory::Ad {
                id: mesh_id.clone(),
                peers: live_count.load(Ordering::Relaxed),
            };
            if let Err(error) = session.send(ad.to_body()).await {
                tracing::debug!(
                    target: "agent_habilis_mesh::directory",
                    %error,
                    "directory advertise: re-broadcast failed (will retry next tick)"
                );
            }
        }
    }))
}
