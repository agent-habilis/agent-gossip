use std::fmt;
use std::sync::atomic::Ordering;

use habilis_network::ops::directory;
use habilis_network::ops::directory::directory_mesh;
use habilis_network::protocol::{LookupOpts, MeshId, MeshName};
pub(crate) use habilis_network::runtime::DIRECTORY_ADVERTISER_COHOST;
use habilis_network::runtime::tuning::advertise_interval;
use habilis_network::runtime::{EventLoopConfig, LiveCount};
use tokio::task::JoinHandle;

use super::MeshSession;

/// Spawn the directory re-broadcast task for `cfg`'s mesh: wire a fresh
/// live-peer counter into `cfg.live_count`, then re-send the
/// mesh's id (with that count) into `directory` every
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

/// The count starts at one: the advertiser itself is a live peer.
pub(crate) fn initial_live_count() -> LiveCount {
    let live_count = LiveCount::default();
    live_count.store(1, Ordering::Relaxed);
    live_count
}

pub(crate) fn spawn_advertiser(
    config: &EventLoopConfig,
    live_count: LiveCount,
    directory: MeshName,
    lookups: LookupOpts,
) -> Advertiser {
    let mesh_id = config.mesh_id().clone();
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
                    target: "agent_gossip::directory",
                    %error,
                    directory = %directory,
                    "directory advertise: could not join the directory; gossip stays unlisted"
                );
                return;
            }
        };
        let mut ticker = tokio::time::interval(advertise_interval());
        loop {
            ticker.tick().await;
            let ad = ad_with_peers(&mesh_id, live_count.load(Ordering::Relaxed));
            if let Err(error) = session.broadcast(ad.to_body()).await {
                tracing::debug!(
                    target: "agent_gossip::directory",
                    %error,
                    "directory advertise: re-broadcast failed (will retry next tick)"
                );
            }
        }
    }))
}

/// `Ad` has no constructor, so the ad is built through its validating `Deserialize`.
fn ad_with_peers(id: &MeshId, peers: usize) -> directory::Ad {
    serde_json::from_value(serde_json::json!({ "id": id, "peers": peers }))
        .expect("a mesh id and a peer count make a valid ad")
}
