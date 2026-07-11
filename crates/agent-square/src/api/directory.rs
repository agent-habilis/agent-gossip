use agent_habilis_mesh::daemon::CoHostPolicy;
use agent_habilis_mesh::protocol::mesh::{DEFAULT_DIRECTORY, resolve_lookups};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::MeshSession;
use agent_habilis_mesh::directory::{Listing, ListingChange, Listings, directory_mesh};
use agent_habilis_mesh::protocol::mesh::{LookupSet, MeshName};
use agent_habilis_mesh::protocol::{MeshId, Nickname};
use agent_habilis_mesh::util::tuning::directory_expiry_secs;

// ── Directory (directory consumer) ─────────────────────────────────────

/// One live directory entry handed to embedders — the public, iroh-free
/// projection of a `agent_habilis_mesh::directory::Listing`.
#[derive(Debug, Clone)]
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
    /// Live participant count from the most recent ad.
    pub peers: usize,
    /// Unix seconds when this mesh was first seen in the directory
    /// (stable across re-ads).
    pub first_seen_unix: i64,
}

/// A directory change observed by a [`Directory`].
#[derive(Debug, Clone)]
pub enum DirectoryEvent {
    /// A mesh appeared in the directory.
    Found(MeshListing),
    /// An already-listed mesh re-advertised (refreshed count/freshness).
    Updated(MeshListing),
    /// A mesh's ads stopped and its listing aged out.
    Lost(MeshId),
}

fn public_listing(listing: &Listing) -> MeshListing {
    MeshListing {
        mesh: listing.mesh.clone(),
        name: listing.name.clone(),
        public: listing.public,
        password: listing.password,
        peers: listing.peers,
        first_seen_unix: listing.first_seen_unix,
    }
}

/// A live view of a directory. Joins the directory's mesh as an
/// ordinary [`MeshSession`] and collects advertisements into
/// `Listings`, aging out meshes whose publishers went silent. Drop
/// (or let it fall out of scope) to leave the directory.
///
/// ```no_run
/// # use agent_square::api::Directory;
/// # use agent_square::LookupSet;
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
    session: Option<MeshSession>,
    listings: Arc<Mutex<Listings>>,
    events_rx: Option<mpsc::UnboundedReceiver<DirectoryEvent>>,
    task: Option<JoinHandle<()>>,
}

impl Directory {
    /// Open a directory by name, reaching it over `lookups`. `name` is the
    /// directory name; `None` ⇒ the well-known `global` directory. The
    /// directory's topic is keyed by name **and** the lookups in use, so a
    /// discoverer only sees advertisers that reached the directory over the
    /// **same** lookups; bare `LookupSet::default()` resolves to the all-on
    /// preset (mDNS + DHT + relay), matching a `--public` advertiser. A
    /// disabled leg issues no network requests for the directory. Returns once
    /// the directory session is ready; listings then accumulate in the
    /// background.
    ///
    /// # Errors
    /// Fails if the name is invalid or the directory session cannot be
    /// established (endpoint/gossip setup, bootstrap unreachable).
    ///
    /// # Panics
    /// Panics only if the internal collector mutex is poisoned by a
    /// panic in the background task — not reachable in normal use.
    pub async fn open(name: Option<impl Into<String>>, lookups: LookupSet) -> anyhow::Result<Self> {
        let directory_name = match name {
            Some(value) => MeshName::new(value.into())
                .map_err(|error| anyhow::anyhow!("invalid directory name: {error}"))?,
            None => {
                MeshName::new(DEFAULT_DIRECTORY).expect("DEFAULT_DIRECTORY is a valid mesh name")
            }
        };
        // Directories are inherently networked, so resolve as if `--public`:
        // no flags ⇒ all-on. The test env forces loopback so the hermetic
        // advertise→discover path runs without the public relay.
        let resolved = resolve_lookups(
            !agent_habilis_mesh::util::tuning::directory_private_for_test(),
            lookups,
        );
        let mesh = directory_mesh(&directory_name, resolved);
        // A discoverer is a pure consumer: never co-host the directory's
        // rendezvous (it only dials an advertiser's beacon).
        let session = MeshSession::join_decoded(mesh, None, CoHostPolicy::Never).await?;
        let mut inbound = session.messages();
        let listings = Arc::new(Mutex::new(Listings::new()));
        let (events_tx, events_rx) = mpsc::unbounded_channel();

        let collector = listings.clone();
        let task = tokio::spawn(async move {
            let ttl = Duration::from_secs(directory_expiry_secs());
            let mut expiry = tokio::time::interval(ttl);
            expiry.tick().await; // eat the immediate first tick
            loop {
                tokio::select! {
                    received = inbound.recv() => match received {
                        Ok(message) => {
                            let now = Instant::now();
                            let event = {
                                let mut dir = collector.lock().expect("directory mutex not poisoned");
                                // Ads ride broadcast chat frames, so the ad
                                // text is the payload's text projection, not
                                // the raw frame body (a serialized A2A object).
                                let Some(ad_text) = crate::a2a::gossip::chat_text(&message) else {
                                    continue;
                                };
                                match dir.note(&ad_text, now) {
                                    Some(ListingChange::Found(id)) => dir
                                        .get(&id)
                                        .map(|listing| DirectoryEvent::Found(public_listing(listing))),
                                    Some(ListingChange::Updated(id)) => dir
                                        .get(&id)
                                        .map(|listing| DirectoryEvent::Updated(public_listing(listing))),
                                    None => None,
                                }
                            };
                            if let Some(event) = event {
                                let _ = events_tx.send(event);
                            }
                        }
                        // Slow consumer dropped some inbound — listings
                        // self-heal on the next re-ad, so skip and continue.
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                    _ = expiry.tick() => {
                        let now = Instant::now();
                        let lost = {
                            let mut dir = collector.lock().expect("directory mutex not poisoned");
                            dir.expire(ttl, now)
                        };
                        for id in lost {
                            let _ = events_tx.send(DirectoryEvent::Lost(id));
                        }
                    }
                }
            }
        });

        Ok(Self {
            session: Some(session),
            listings,
            events_rx: Some(events_rx),
            task: Some(task),
        })
    }

    /// The current live listings, sorted by name then id.
    ///
    /// # Panics
    /// Panics only if the internal collector mutex was poisoned by a
    /// prior panic in the background task — not reachable in normal use.
    #[must_use]
    pub fn snapshot(&self) -> Vec<MeshListing> {
        self.listings
            .lock()
            .expect("directory mutex not poisoned")
            .snapshot()
            .iter()
            .map(public_listing)
            .collect()
    }

    /// Take the directory event stream (`Found` / `Updated` / `Lost`).
    /// Single-consumer, so this returns the receiver **once**;
    /// subsequent calls return `None`.
    pub fn events(&mut self) -> Option<mpsc::UnboundedReceiver<DirectoryEvent>> {
        self.events_rx.take()
    }

    /// The directory session's `(mesh id, nickname)` while it is open —
    /// used by the CLI to route its logs to the per-member file via
    /// `logging::attach` (so the picker / JSON stream stays clean).
    pub(crate) fn session_identity(&self) -> Option<(&MeshId, &Nickname)> {
        self.session
            .as_ref()
            .map(|session| (session.mesh_id(), session.nickname()))
    }

    /// Leave the directory and stop collecting.
    ///
    /// # Errors
    /// Propagates a clean-shutdown error from the underlying directory
    /// [`MeshSession::leave`] (event-loop task panic / loop error).
    pub async fn close(mut self) -> anyhow::Result<()> {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        if let Some(session) = self.session.take() {
            session.leave().await?;
        }
        Ok(())
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        // The directory `MeshSession` (if not already taken by `close`)
        // drops here, winding down its own loop.
    }
}
