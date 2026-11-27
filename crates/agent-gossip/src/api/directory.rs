use std::time::Instant;

use habilis_network::ops::directory::{self, Listing, ListingChange, Listings};
use habilis_network::protocol::{
    DEFAULT_DIRECTORY, LookupOpts, LookupSet, MeshId, MeshName, MessageBody, Nickname,
    resolve_lookups,
};
use habilis_network::runtime::{CoHostPolicy, tuning};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use super::directory_error::DirectoryError;
use super::{DIRECTORY_ADVERTISER_COHOST, MeshSession, event_sink};
use crate::a2a::status_update_params;

// ── Directory (directory consumer) ─────────────────────────────────────

/// One live directory entry handed to embedders — the public, iroh-free
/// projection of a `habilis_network::ops::directory::Listing`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct MeshListing {
    /// The advertised mesh's id — pass to [`MeshSession::join`] to join.
    pub mesh: MeshId,
    /// Human-readable mesh name (decoded from the id).
    pub name: MeshName,
    /// `true` if the mesh is on the public network.
    pub public: bool,
    /// `true` if the mesh id carries a password verifier — joining needs
    /// the password, so the listing alone does not admit.
    pub password: bool,
    /// Live peer count from the most recent ad.
    pub peers: usize,
    /// Unix seconds when this mesh was first seen in the directory
    /// (stable across re-ads).
    pub first_seen_unix: i64,
}

/// A directory change observed by a [`Directory`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum DirectoryEvent {
    /// A mesh appeared in the directory.
    Found(MeshListing),
    /// An already-listed mesh re-advertised (refreshed count/freshness).
    Updated(MeshListing),
    /// A mesh's ads stopped and its listing aged out.
    Lost(MeshId),
    /// The feed dropped this many events since the last one it sent, so the listing may be stale:
    /// list again.
    EventsLost(u64),
}

impl event_sink::Evictable for DirectoryEvent {
    fn is_lost(&self) -> bool {
        matches!(self, DirectoryEvent::Lost(_))
    }

    fn events_lost(count: u64) -> Self {
        DirectoryEvent::EventsLost(count)
    }
}

fn public_listing(listing: &Listing) -> MeshListing {
    MeshListing {
        mesh: listing.mesh.clone(),
        name: listing.name.clone(),
        public: listing.public,
        password: listing.password,
        peers: listing.peers,
        first_seen_unix: listing.first_seen_unix.as_secs(),
    }
}

/// The collector task owns the listings; the handle reads this published copy,
/// so no lock is shared between them.
fn publish_snapshot(snapshot_tx: &watch::Sender<Vec<MeshListing>>, listings: &Listings) {
    snapshot_tx.send_replace(
        listings
            .snapshot()
            .map(|listing| public_listing(&listing))
            .collect(),
    );
}

/// The task that collects a directory's ads. Aborts rather than detaches: a bare
/// `JoinHandle` dropped on the floor keeps collecting. A field with its own `Drop`
/// also lets `close` move the session out of its owner.
#[derive(Debug)]
pub(crate) struct Collector(JoinHandle<()>);

impl Collector {
    pub(crate) fn spawn(collect: impl Future<Output = ()> + Send + 'static) -> Self {
        Self(tokio::spawn(collect))
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Join `directory` over `lookups` and broadcast `next_ad()` into it at every
/// advertise interval, until the future is dropped. A directory-join failure
/// logs and ends it: whatever the ad lists is unaffected, only unlisted.
pub(crate) async fn advertise(
    directory: MeshName,
    lookups: LookupOpts,
    mut next_ad: impl FnMut() -> MessageBody + Send,
) {
    let mesh = directory::directory_mesh(&directory, lookups);
    // Co-host the directory rendezvous from t=0 so a beacon exists
    // before any discoverer subscribes (a `Deferred` advertiser would
    // only beacon at the first heal tick, after discoverers already
    // failed their first graft). Probe-first; see DIRECTORY_ADVERTISER_COHOST.
    let session = match MeshSession::join_decoded(mesh, None, DIRECTORY_ADVERTISER_COHOST).await {
        Ok(session) => session,
        Err(error) => {
            tracing::warn!(
                target: "agent_gossip::directory",
                %error,
                directory = %directory,
                "directory advertise: could not join the directory; the ad stays unlisted"
            );
            return;
        }
    };
    let mut ticker = tokio::time::interval(tuning::advertise_interval());
    loop {
        ticker.tick().await;
        if let Err(error) = session.broadcast(next_ad()).await {
            tracing::debug!(
                target: "agent_gossip::directory",
                %error,
                "directory advertise: re-broadcast failed (will retry next tick)"
            );
        }
    }
}

/// A live view of a directory.
///
/// Joins the directory's mesh as an
/// ordinary [`MeshSession`] and collects advertisements into
/// `Listings`, aging out meshes whose publishers went silent. Drop
/// (or let it fall out of scope) to leave the directory.
///
/// ```no_run
/// # use agent_gossip::api::Directory;
/// # use agent_gossip::LookupSet;
/// # async fn run() -> anyhow::Result<()> {
/// // Bare `LookupSet::default()` ⇒ all-on (mDNS + DHT + relay).
/// let mut directory = Directory::open(Some("demo"), LookupSet::default()).await?;
/// for listing in directory.snapshot() {
///     println!("#{} — {} peers — {}", listing.name, listing.peers, listing.mesh.as_str());
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct Directory {
    // First, so that an implicit drop stops the collector before the session, as `close` does.
    collector: Collector,
    session: MeshSession,
    snapshot_rx: watch::Receiver<Vec<MeshListing>>,
    /// `None` after [`Directory::events`] hands the receiver out.
    events_rx: Option<event_sink::EventReceiver<DirectoryEvent>>,
}

impl Directory {
    /// Open a directory by name, reaching it over `lookups`. `name` is the
    /// directory name; `None` ⇒ the well-known `global` directory. The
    /// directory's topic is keyed by name **and** the lookups in use, so a
    /// discoverer only sees advertisers that reached the directory over the
    /// **same** lookups; bare `LookupSet::default()` resolves to the all-on
    /// preset (mDNS + DHT + relay), the CLI `discover` default. An advertiser
    /// takes the same all-on default when `create` names no `--lookup`, so it
    /// meets this session; an advertiser on `--lookup loopback` cannot
    /// advertise. A disabled leg issues no network requests
    /// for the directory. Returns once
    /// the directory session is ready; listings then accumulate in the
    /// background.
    ///
    /// # Errors
    /// Fails if the name is invalid or the directory session cannot be
    /// established (endpoint/gossip setup, bootstrap unreachable).
    ///
    /// # Panics
    /// Panics only if `DEFAULT_DIRECTORY` is not a valid mesh name — not
    /// reachable, it is a constant.
    pub async fn open(name: Option<&str>, lookups: LookupSet) -> Result<Self, DirectoryError> {
        let directory_name = match name {
            Some(value) => MeshName::try_new(value)
                .map_err(|error| DirectoryError::InvalidName(error.to_string()))?,
            None => MeshName::try_new(DEFAULT_DIRECTORY)
                .expect("DEFAULT_DIRECTORY is a valid mesh name"),
        };
        // Directories are inherently networked, so resolve as all-on:
        // no flags ⇒ all-on. The test env forces loopback so the hermetic
        // advertise→discover path runs without the public relay.
        let resolved = resolve_lookups_or_public(!tuning::directory_private_for_test(), lookups);
        let mesh = directory::directory_mesh(&directory_name, resolved);
        // A discoverer is a pure consumer: never co-host the directory's
        // rendezvous (it only dials an advertiser's beacon).
        let session = MeshSession::join_decoded(mesh, None, CoHostPolicy::Never)
            .await
            .map_err(DirectoryError::Join)?;
        let mut inbound = session.messages();
        let mut listings = Listings::new();
        let (snapshot_tx, snapshot_rx) = watch::channel(Vec::new());
        let (events_tx, events_rx) = event_sink::event_feed();

        let collector = Collector::spawn(async move {
            let ttl = tuning::directory_expiry();
            let mut expiry = tokio::time::interval(ttl);
            expiry.tick().await; // eat the immediate first tick
            loop {
                tokio::select! {
                    received = inbound.recv() => match received {
                        Ok(message) => {
                            let now = Instant::now();
                            // Ads ride broadcast chat frames, so the ad
                            // text is the payload's text projection, not
                            // the raw frame body (a serialized A2A object).
                            let Some(ad_text) = status_update_params::chat_text(&message) else {
                                continue;
                            };
                            let event: Option<DirectoryEvent> = match listings.note(&ad_text, now) {
                                Some(ListingChange::Found(id)) => listings
                                    .get(&id)
                                    .map(|listing| DirectoryEvent::Found(public_listing(listing))),
                                Some(ListingChange::Updated(id)) => listings
                                    .get(&id)
                                    .map(|listing| DirectoryEvent::Updated(public_listing(listing))),
                                _ => None,
                            };
                            publish_snapshot(&snapshot_tx, &listings);
                            if let Some(event) = event {
                                events_tx.publish(event);
                            }
                        }
                        // Slow consumer dropped some inbound — listings
                        // self-heal on the next re-ad, so skip and continue.
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                    _ = expiry.tick() => {
                        let now = Instant::now();
                        let lost: Vec<_> = listings.expire(ttl, now).collect();
                        publish_snapshot(&snapshot_tx, &listings);
                        for id in lost {
                            events_tx.publish(DirectoryEvent::Lost(id));
                        }
                    }
                }
            }
        });

        Ok(Self {
            collector,
            session,
            snapshot_rx,
            events_rx: Some(events_rx),
        })
    }

    /// The current live listings, sorted by name then id.
    pub fn snapshot(&self) -> impl Iterator<Item = MeshListing> {
        self.snapshot_rx.borrow().clone().into_iter()
    }

    /// Take the directory event stream (`Found` / `Updated` / `Lost`).
    /// Single-consumer, so this returns the receiver **once**;
    /// subsequent calls return `None`.
    pub fn events(&mut self) -> Option<event_sink::EventReceiver<DirectoryEvent>> {
        self.events_rx.take()
    }

    /// The directory session's `(mesh id, nickname)` —
    /// used by the CLI to route its logs to the per-member file via
    /// `logging::attach` (so the picker / JSON stream stays clean).
    pub(crate) fn session_identity(&self) -> (&MeshId, &Nickname) {
        (self.session.mesh_id(), self.session.nickname())
    }

    /// Leave the directory and stop collecting.
    ///
    /// # Errors
    /// Propagates a clean-shutdown error from the underlying directory
    /// [`MeshSession::leave`] (event-loop task panic / loop error).
    pub async fn close(self) -> Result<(), DirectoryError> {
        drop(self.collector);
        self.session.leave().await.map_err(DirectoryError::Leave)
    }
}

/// habilis-network's `resolve_lookups` with a `public` default: a networked
/// surface (a directory, a bridge) that names no lookup gets the all-on public
/// preset. habilis-network moved that choice out to its callers. The lookups are
/// baked into the gossip id, so two peers that resolve them differently land
/// in different directories.
pub(crate) fn resolve_lookups_or_public(public: bool, lookups: LookupSet) -> LookupOpts {
    if public && !lookups.any() {
        LookupOpts::public_preset()
    } else {
        resolve_lookups(lookups)
    }
}

#[cfg(test)]
mod lookup_tests {
    use habilis_network::ops::directory;
    use habilis_network::protocol::{DEFAULT_DIRECTORY, LookupSet, MeshName};

    fn directory_topic(public: bool, lookups: LookupSet) -> String {
        let name =
            MeshName::try_new(DEFAULT_DIRECTORY).expect("the test literal is a valid mesh name");
        format!(
            "{:?}",
            directory::directory_mesh(&name, super::resolve_lookups_or_public(public, lookups))
                .topic_id()
        )
    }

    /// The directory's gossip id may not move by accident when the habilis-network
    /// pin moves. The values moved once on purpose, with mesh id version 2
    /// (the release is breaking: a peer on version 1 lands in another
    /// directory). Create, join and topic need no such pin: they never passed
    /// `public`, so they resolve as before by construction.
    #[test]
    fn directory_ids_survive_the_lookup_api_change() {
        assert_eq!(
            directory_topic(true, LookupSet::default()),
            "TopicId(0ac3690b9968f8fd7f2183337984af2629df292a94165f0405135ed001db0dc7)",
            "the directory topic id is stable across the lookup API"
        );
        let mut mdns_only = LookupSet::default();
        mdns_only.mdns = true;
        assert_eq!(
            directory_topic(true, mdns_only),
            "TopicId(aaef8fe2a5ca5560cdecc153d4ee18d446e6448d831e97e1639e1ef8b6b42eaa)",
            "the directory topic id is stable across the lookup API"
        );
        assert_eq!(
            directory_topic(false, LookupSet::default()),
            "TopicId(14a8001b57bb1cbcc7b94f936c71cb94c2ab39a8c9b0423dbad0da809f93db8f)",
            "the directory topic id is stable across the lookup API"
        );
    }
}
