//! The `discover` subcommand: browse a directory's live meshes.
//!
//! Streams `gossip_found`/`gossip_lost` JSON lines for an agent to act on
//! (the agent picks and joins by id itself). The pure directory primitives
//! live in [`agent_habilis_mesh::directory`]; the live consumer in
//! [`crate::api::Directory`]; this file is just the CLI command + mesh
//! rendering.

use anyhow::Result;

use crate::api::{Directory, DirectoryEvent};

use super::args::DiscoverOpts;
use super::signal::{interrupted, sigterm_stream};

/// Browse a directory, streaming `gossip_found`/`gossip_lost` JSON lines until
/// interrupted (SIGINT or SIGTERM) or, with `--window-secs`, until the window
/// closes.
pub(super) async fn discover(opts: DiscoverOpts) -> Result<()> {
    // Reach the directory over the chosen lookups (default all-on). Must
    // match the lookups the advertiser used — an mDNS-only advertiser is
    // found only by an mDNS-only `discover`.
    let mut discoverer = Directory::open(
        opts.directory.map(|name| name.as_str().to_owned()),
        opts.lookups.to_set(),
    )
    .await?;
    // Route the directory session's logs to its per-member file (same as
    // create/join) so the JSON stream isn't drowned in INFO lines on stderr.
    if let Some((mesh, nickname)) = discoverer.session_identity() {
        agent_habilis_mesh::logging::attach(mesh, nickname);
    }
    let mut events = discoverer
        .events()
        .expect("discoverer events receiver is available exactly once");

    // Pinned once: recreating the sleep inside `select!` would restart the
    // window on every event.
    let window = async {
        match opts.window_secs {
            Some(secs) => tokio::time::sleep(std::time::Duration::from_secs(secs)).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(window);

    // One JSON line per directory change until interrupted (SIGINT or
    // SIGTERM — see `interrupted`) or the collection window closes.
    let mut sigterm = sigterm_stream();
    loop {
        tokio::select! {
            () = interrupted(&mut sigterm) => break,
            () = &mut window => {
                // The window bounds listening, not printing: flush events
                // already buffered in the channel so a listing that arrived
                // moments before the deadline is not silently dropped.
                while let Ok(change) = events.try_recv() {
                    println!("{}", discover_event_json(&change));
                }
                break;
            }
            received = events.recv() => match received {
                Some(change) => println!("{}", discover_event_json(&change)),
                None => break,
            },
        }
    }
    let _ = discoverer.close().await;
    Ok(())
}

/// One directory change as a JSON line for `agent-gossip discover`.
/// `Found`/`Updated` both surface as `gossip_found` (upsert semantics —
/// the agent treats a re-ad as a refresh); a departure is `gossip_lost`.
fn discover_event_json(event: &DirectoryEvent) -> String {
    let value = match event {
        DirectoryEvent::Found(listing) | DirectoryEvent::Updated(listing) => serde_json::json!({
            "event": "gossip_found",
            "gossip": listing.mesh.as_str(),
            "name": listing.name,
            "mode": if listing.public { "public" } else { "private" },
            "password": listing.password,
            "peers": listing.peers,
        }),
        DirectoryEvent::Lost(mesh) => serde_json::json!({
            "event": "gossip_lost",
            "gossip": mesh.as_str(),
        }),
    };
    value.to_string()
}
