//! The directory — opt-in swarm discovery ("swarms all the way
//! down").
//!
//! A swarm created with `--advertise[=<name>]` re-broadcasts its own
//! `ahs…` id into a **directory**; `ahs discover` browses it. A directory
//! is not a server — it is itself a well-known public [`Swarm`] derived
//! deterministically from its name, so a publisher and a discoverer that
//! name the same directory derive the same swarm and mesh over the
//! ordinary gossip + relay/DHT/mDNS stack. Ads are plain gossip messages
//! ([`Ad`] in the body) on that swarm's topic — there is no stored list,
//! so an ad lives only while its publisher keeps re-broadcasting (see
//! [`crate::util::tuning`]'s `ADVERTISE_INTERVAL_SECS` /
//! `DIRECTORY_EXPIRY_SECS`).
//!
//! This module is **pure**: directory derivation, the [`Ad`] codec, and the
//! [`Listings`] collector. The advertise *task* (a live
//! [`crate::embed::SwarmSession`] on the directory) lives in `embed`, and
//! the discover UI in `cli` — both drive the primitives here.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::protocol::crypto::kdf;
use crate::protocol::swarm::{LookupOpts, Swarm, SwarmConfig, SwarmName};
use crate::protocol::{MessageBody, SwarmId};

/// Domain-separation seed for every directory. The directory name is
/// the `kdf` *label*; this is the *seed*, so a directory's derived swarm
/// seed (`kdf(DIRECTORY_BASE_SEED, directory)`) is a SHA-256 output that can
/// never collide with a user swarm's random 32-byte seed. Bumping this
/// orphans every existing directory (a wire-incompatible directory change).
const DIRECTORY_BASE_SEED: [u8; 32] = *b"agent-habilis-swarm/directory/v1";

/// The well-known [`Swarm`] for a directory, reached over `lookups`. Both
/// `--advertise <name>` and `ahs discover --directory <name>` call this; the
/// seed + rendezvous are name-derived (so they're identical regardless of
/// `lookups`), but the **topic** mixes in the config bytes — which include the
/// lookups — so an advertiser and a discoverer meet only when they pass the
/// **same** `lookups`. That is deliberate: the directory's lookups are the
/// advertiser's own swarm lookups (so an mDNS-only swarm advertises over mDNS
/// only) and the discoverer's `--mdns/--dht/--relay` choice, and the two must
/// align to see each other. A different name still yields a fully independent
/// directory.
pub(crate) fn directory_swarm(directory: &SwarmName, lookups: LookupOpts) -> Swarm {
    Swarm::new(
        kdf(&DIRECTORY_BASE_SEED, directory.as_bytes()),
        directory.clone(),
        directory_config(lookups),
    )
}

/// The config a directory swarm uses for `lookups`: the standard rate limit
/// (fixed, so the topic varies only with the lookups) plus the caller's chosen
/// lookups. The lookups become the directory session's lookups, so a member
/// reaches the directory over exactly those mechanisms — a disabled leg issues
/// no network requests for the directory at all.
pub(crate) fn directory_config(lookups: LookupOpts) -> SwarmConfig {
    SwarmConfig {
        rate_limit_per_min: ahs_shared::RATE_LIMIT_PER_MIN,
        lookups,
    }
}

/// A directory advertisement: the advertised swarm's `ahs…` id plus its
/// live participant count. The id already encodes the swarm name and
/// network mode, so a discoverer decodes those locally — nothing else need
/// be on the wire. Serialized as a JSON object (room for future fields;
/// discoverers ignore unknown keys via serde's default behaviour and
/// ignore unparseable bodies entirely).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Ad {
    pub id: SwarmId,
    pub peers: usize,
}

impl Ad {
    /// Render this ad as a [`MessageBody`] for broadcast on the directory.
    /// Infallible: the JSON of an `{id, peers}` object contains no
    /// control characters.
    pub(crate) fn to_body(&self) -> MessageBody {
        let json = serde_json::to_string(self).expect("Ad always serializes to JSON");
        MessageBody::new(json).expect("Ad JSON contains no control characters")
    }

    /// Parse a directory message body as an ad. Returns `None` for any
    /// non-ad body (presence, digests, junk) so the collector can feed
    /// every directory message through without pre-filtering.
    pub(crate) fn parse(body: &str) -> Option<Self> {
        serde_json::from_str(body).ok()
    }
}

/// One live directory entry, decoded from an [`Ad`].
#[derive(Debug, Clone)]
pub(crate) struct Listing {
    pub swarm: SwarmId,
    pub name: SwarmName,
    /// `true` if the advertised swarm's id decodes to the public
    /// network (the norm — `--advertise` requires `--public`).
    pub public: bool,
    pub peers: usize,
    /// Local instant of the most recent ad; drives expiry.
    pub last_seen: Instant,
    /// Unix seconds when this swarm was *first* seen in the directory
    /// (preserved across re-ads). Display-only — the `ahs discover` picker
    /// renders it as an ISO-8601 timestamp.
    pub first_seen_unix: i64,
}

/// The change one [`Listings::observe`] made: a newly seen swarm or a
/// refreshed peer count. Departures aren't here — they come from
/// [`Listings::expire`], which returns the aged-out ids directly. The
/// embed layer maps this onto the public [`crate::embed::DirectoryEvent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ListingChange {
    /// A swarm seen for the first time.
    Found(SwarmId),
    /// A re-ad whose visible data (peer count) changed. A re-ad with an
    /// unchanged count produces no event — see [`Listings::observe`].
    Updated(SwarmId),
}

/// Upper bound on tracked listings. The directory is an open public mesh
/// (anyone can mint and broadcast valid `ahs…` ids), so the map is
/// capped — a new id past the cap evicts the stalest entry — mirroring
/// the bounded-set discipline the rest of the daemon follows for
/// adversary-reachable collections.
const MAX_LISTINGS: usize = 256;

/// The live directory: a set of [`Listing`]s keyed by swarm id, fed by
/// directory messages and aged out by [`Listings::expire`]. Pure +
/// deterministic (the caller supplies `now`), so it unit-tests without
/// a clock or a network. Shared by the CLI `discover` picker and the embed
/// [`crate::embed::Directory`].
#[derive(Debug, Default)]
pub(crate) struct Listings {
    entries: HashMap<SwarmId, Listing>,
}

impl Listings {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Process one directory message body. A valid [`Ad`] whose id
    /// decodes to a [`Swarm`] refreshes the listing's liveness; the
    /// returned event is `Found` for a new swarm, `Updated` only when a
    /// visible field (the peer count) changed, and `None` for an
    /// unchanged re-ad or an unparseable body. Suppressing the no-op
    /// `Updated` matters because every advertiser re-ads on a fixed
    /// interval — surfacing each would repaint the picker / spam the
    /// JSON stream every tick with identical data.
    pub(crate) fn observe(&mut self, body: &str, now: Instant) -> Option<ListingChange> {
        let ad = Ad::parse(body)?;
        // `ad.id` is a `SwarmId` (shallow charset check only); the
        // structural `Swarm` decode below is the real validity gate, so
        // an id that doesn't decode is dropped here.
        let swarm: Swarm = ad.id.as_str().parse().ok()?;
        let swarm_id = ad.id;

        if let Some(listing) = self.entries.get_mut(&swarm_id) {
            listing.last_seen = now;
            if listing.peers == ad.peers {
                return None;
            }
            listing.peers = ad.peers;
            return Some(ListingChange::Updated(swarm_id));
        }

        // New swarm. Bound the map by evicting the stalest entry.
        if self.entries.len() >= MAX_LISTINGS
            && let Some(stalest) = self
                .entries
                .iter()
                .min_by_key(|(_, listing)| listing.last_seen)
                .map(|(id, _)| id.clone())
        {
            self.entries.remove(&stalest);
        }
        self.entries.insert(
            swarm_id.clone(),
            Listing {
                swarm: swarm_id.clone(),
                public: !swarm.is_loopback(),
                name: swarm.name,
                peers: ad.peers,
                last_seen: now,
                first_seen_unix: crate::util::clock::unix_secs(),
            },
        );
        Some(ListingChange::Found(swarm_id))
    }

    /// Drop every listing whose last ad is older than `ttl`, returning
    /// the ids removed (so the caller can surface a `Lost` event each).
    pub(crate) fn expire(&mut self, ttl: Duration, now: Instant) -> Vec<SwarmId> {
        let mut expired = Vec::new();
        self.entries.retain(|id, listing| {
            let alive = now.duration_since(listing.last_seen) <= ttl;
            if !alive {
                expired.push(id.clone());
            }
            alive
        });
        expired
    }

    /// The current listings, sorted by swarm name then id — a stable
    /// order for the picker and for `snapshot()` callers.
    pub(crate) fn snapshot(&self) -> Vec<Listing> {
        let mut listings: Vec<Listing> = self.entries.values().cloned().collect();
        listings.sort_by(|left, right| {
            left.name
                .as_str()
                .cmp(right.name.as_str())
                .then_with(|| left.swarm.as_str().cmp(right.swarm.as_str()))
        });
        listings
    }

    /// Look up a single listing by id (used to attach data to an event).
    pub(crate) fn get(&self, id: &SwarmId) -> Option<&Listing> {
        self.entries.get(id)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{Ad, ListingChange, Listings, directory_swarm};
    use crate::protocol::SwarmId;
    use crate::protocol::swarm::{LookupOpts, SwarmName};

    fn directory(name: &str) -> SwarmName {
        SwarmName::new(name).unwrap()
    }

    /// The advertised swarm's id for a directory of the given name.
    fn advertised_id(name: &str) -> SwarmId {
        SwarmId::new(directory_swarm(&directory(name), LookupOpts::public_preset()).to_string())
            .expect("directory swarm id is valid")
    }

    #[test]
    fn directory_is_deterministic_per_name() {
        let one = directory_swarm(&directory("global"), LookupOpts::public_preset());
        let two = directory_swarm(&directory("global"), LookupOpts::public_preset());
        assert_eq!(
            one.to_string(),
            two.to_string(),
            "same directory + same lookups ⇒ same swarm"
        );
    }

    #[test]
    fn distinct_names_are_distinct_directories() {
        assert_ne!(
            directory_swarm(&directory("global"), LookupOpts::public_preset()).to_string(),
            directory_swarm(&directory("gamedev"), LookupOpts::public_preset()).to_string(),
            "different names ⇒ independent directories"
        );
    }

    #[test]
    fn directory_is_public_with_public_lookups() {
        assert!(!directory_swarm(&directory("global"), LookupOpts::public_preset()).is_loopback());
    }

    #[test]
    fn directory_topic_couples_to_lookups() {
        // The symmetry rule: same name but different lookups ⇒ a different
        // directory swarm (id encodes config_bytes, and the topic derives
        // from those same bytes), so an advertiser and a discoverer meet only
        // when their lookups match. Equal lookups ⇒ identical swarm.
        let name = directory("global");
        let all_on = directory_swarm(&name, LookupOpts::public_preset()).to_string();
        let all_on_again = directory_swarm(&name, LookupOpts::public_preset()).to_string();
        let loopback = directory_swarm(&name, LookupOpts::loopback()).to_string();
        assert_eq!(
            all_on, all_on_again,
            "same name + same lookups ⇒ same directory"
        );
        assert_ne!(
            all_on, loopback,
            "same name, different lookups ⇒ different directory (they won't meet)"
        );
    }

    #[test]
    fn ad_round_trips_through_body() {
        // Build a real advertised id so `observe` can decode it.
        let advertised = advertised_id("demo");
        let ad = Ad {
            id: advertised.clone(),
            peers: 4,
        };
        let body = ad.to_body();
        let parsed = Ad::parse(body.as_str()).expect("ad parses");
        assert_eq!(parsed.id, advertised);
        assert_eq!(parsed.peers, 4);
    }

    #[test]
    fn parse_rejects_non_ad_bodies() {
        assert!(Ad::parse("").is_none());
        assert!(Ad::parse("not json").is_none());
        assert!(
            Ad::parse(r#"["a","b"]"#).is_none(),
            "digest array isn't an ad"
        );
        assert!(Ad::parse(r#"{"peers":1}"#).is_none(), "missing id");
    }

    #[test]
    fn observe_found_then_updated_then_expire_lost() {
        let advertised = advertised_id("demo");
        let body = Ad {
            id: advertised.clone(),
            peers: 2,
        }
        .to_body();

        let mut dir = Listings::new();
        let start = Instant::now();

        // First sighting ⇒ Found.
        let first_event = dir.observe(body.as_str(), start);
        assert!(matches!(first_event, Some(ListingChange::Found(_))));
        let listing = &dir.snapshot()[0];
        assert_eq!(listing.name.as_str(), "demo");
        assert_eq!(listing.peers, 2);
        assert!(listing.public);

        // Re-ad with a changed peer count ⇒ Updated.
        let refreshed = Ad {
            id: advertised,
            peers: 5,
        }
        .to_body();
        let second_event = dir.observe(refreshed.as_str(), start + Duration::from_secs(20));
        assert!(matches!(second_event, Some(ListingChange::Updated(_))));
        assert_eq!(dir.snapshot()[0].peers, 5);

        // No fresh ad past the ttl ⇒ expired/Lost.
        let lost = dir.expire(Duration::from_mins(1), start + Duration::from_secs(200));
        assert_eq!(lost.len(), 1);
        assert!(dir.snapshot().is_empty());
    }

    #[test]
    fn unchanged_re_ad_refreshes_liveness_without_an_event() {
        let body = Ad {
            id: advertised_id("demo"),
            peers: 3,
        }
        .to_body();
        let mut dir = Listings::new();
        let start = Instant::now();

        assert!(matches!(
            dir.observe(body.as_str(), start),
            Some(ListingChange::Found(_))
        ));
        // Same peer count ⇒ no event, but liveness is refreshed so the
        // entry survives an expiry sweep past the original timestamp.
        let later = start + Duration::from_secs(50);
        assert!(dir.observe(body.as_str(), later).is_none());
        assert!(
            dir.expire(Duration::from_mins(1), start + Duration::from_secs(70))
                .is_empty(),
            "the re-ad refreshed last_seen, so the entry is still alive"
        );
        assert_eq!(dir.snapshot().len(), 1);
    }

    #[test]
    fn junk_directory_traffic_is_ignored() {
        let mut dir = Listings::new();
        assert!(dir.observe("", Instant::now()).is_none());
        assert!(dir.observe("hello peers", Instant::now()).is_none());
        assert!(dir.snapshot().is_empty());
    }
}
