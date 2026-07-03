//! Ticket ads — the directory's second citizen beside swarm [`super::Ad`]s.
//!
//! A file producer advertising into a directory broadcasts its full
//! bearer ticket (address + secret), so a discoverer connects with nothing but
//! the pick. The two ad kinds share one directory topic and never cross-parse:
//! a swarm ad's JSON has an `id` key, a ticket ad's a `ticket` key, and each
//! parser requires its own. The collector additionally pins the [`TokenType`]
//! it expects, so `file discover` can never surface a swarm ad.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::protocol::MessageBody;
use crate::protocol::token::{self, TokenType};

use super::MAX_LISTINGS;

/// Whether a decoded ticket payload carries the password flag, per kind.
/// Reads the byte after the 32-byte secret — the file flags byte (bit 0).
/// Unit-tested against the real ticket encoder so the offset can never
/// silently drift.
pub(crate) fn password_bit(kind: TokenType, payload: &[u8]) -> bool {
    let Some(&flags) = payload.get(32) else {
        return false;
    };
    match kind {
        TokenType::File => flags & 0b1 != 0,
        // No password support (yet) for swarm-ad tickets.
        TokenType::Swarm => false,
    }
}

/// Longest label an ad may carry into a listing. The directory is an open
/// public mesh, so a hostile ad must not become an unbounded picker row.
const MAX_LABEL_CHARS: usize = 64;

/// A directory ticket advertisement: the producer's full `🐝…` ticket plus an
/// optional human label (the file name, the exposed ports — whatever the
/// producing command finds useful; the ticket itself is opaque). Serialized as
/// a JSON object like [`super::Ad`]; unknown keys are ignored on parse.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TicketAd {
    pub ticket: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl TicketAd {
    /// Render this ad as a [`MessageBody`] for broadcast on the directory.
    ///
    /// # Errors
    /// The label carries a control character or the JSON exceeds the message
    /// body cap (a ticket is far below it; a caller-supplied label might not be).
    pub(crate) fn to_body(&self) -> anyhow::Result<MessageBody> {
        let json = serde_json::to_string(self)?;
        Ok(MessageBody::new(json)?)
    }

    /// Parse a directory message body as a ticket ad. `None` for any non-ad
    /// body (swarm ads, presence, digests, junk) so the collector can feed
    /// every directory message through without pre-filtering.
    pub(crate) fn parse(body: &str) -> Option<Self> {
        serde_json::from_str(body).ok()
    }
}

/// One live ticket entry, decoded from a [`TicketAd`].
#[derive(Debug, Clone)]
pub(crate) struct TicketListing {
    pub ticket: String,
    pub label: Option<String>,
    /// `true` if the ticket carries the password flag — redeeming it needs
    /// the password, so the ad alone does not admit.
    pub password: bool,
    /// Local instant of the most recent ad; drives expiry.
    pub last_seen: Instant,
    /// Unix seconds when this ticket was *first* seen (preserved across
    /// re-ads). Display-only, like [`super::Listing::first_seen_unix`].
    pub first_seen_unix: i64,
}

/// The change one [`TicketListings::note`] made — mirrors
/// [`super::ListingChange`], keyed by the ticket string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TicketChange {
    /// A ticket seen for the first time.
    Found(String),
    /// A re-ad whose visible data (the label) changed.
    Updated(String),
}

/// The live ticket directory: [`TicketListing`]s keyed by ticket string,
/// pinned to one [`TokenType`] — pure and clock-injected like
/// [`super::Listings`], so it unit-tests without a network.
#[derive(Debug)]
pub(crate) struct TicketListings {
    kind: TokenType,
    entries: HashMap<String, TicketListing>,
}

impl TicketListings {
    pub(crate) fn new(kind: TokenType) -> Self {
        Self {
            kind,
            entries: HashMap::new(),
        }
    }

    /// Process one directory message body. A valid [`TicketAd`] whose ticket
    /// decodes to this collector's token kind refreshes the listing's
    /// liveness; `Found` for a new ticket, `Updated` only when the label
    /// changed, `None` for an unchanged re-ad, a wrong-kind ticket, or an
    /// unparseable body (the same no-op suppression as [`super::Listings`]).
    pub(crate) fn note(&mut self, body: &str, now: Instant) -> Option<TicketChange> {
        let ad = TicketAd::parse(body)?;
        // The structural token decode is the validity gate (checksum + type
        // byte), exactly as `Listings` fully decodes the advertised swarm id.
        let (kind, payload) = token::decode(&ad.ticket).ok()?;
        if kind != self.kind {
            return None;
        }
        let password = password_bit(kind, &payload);
        let label = ad
            .label
            .map(|label| label.chars().take(MAX_LABEL_CHARS).collect::<String>());

        if let Some(listing) = self.entries.get_mut(&ad.ticket) {
            listing.last_seen = now;
            if listing.label == label {
                return None;
            }
            listing.label = label;
            return Some(TicketChange::Updated(ad.ticket));
        }

        // New ticket. Bound the map by evicting the stalest entry.
        if self.entries.len() >= MAX_LISTINGS
            && let Some(stalest) = self
                .entries
                .iter()
                .min_by_key(|(_, listing)| listing.last_seen)
                .map(|(ticket, _)| ticket.clone())
        {
            self.entries.remove(&stalest);
        }
        self.entries.insert(
            ad.ticket.clone(),
            TicketListing {
                ticket: ad.ticket.clone(),
                label,
                password,
                last_seen: now,
                first_seen_unix: crate::util::clock::unix_secs(),
            },
        );
        Some(TicketChange::Found(ad.ticket))
    }

    /// Drop every listing whose last ad is older than `ttl`, returning the
    /// tickets removed (so the caller can surface a `Lost` event each).
    pub(crate) fn expire(&mut self, ttl: Duration, now: Instant) -> Vec<String> {
        let mut expired = Vec::new();
        self.entries.retain(|ticket, listing| {
            let alive = now.duration_since(listing.last_seen) <= ttl;
            if !alive {
                expired.push(ticket.clone());
            }
            alive
        });
        expired
    }

    /// The current listings, sorted by label then ticket — a stable order for
    /// the picker.
    pub(crate) fn snapshot(&self) -> Vec<TicketListing> {
        let mut listings: Vec<TicketListing> = self.entries.values().cloned().collect();
        listings.sort_by(|left, right| {
            left.label
                .cmp(&right.label)
                .then_with(|| left.ticket.cmp(&right.ticket))
        });
        listings
    }

    /// Look up a single listing by ticket (used to attach data to an event).
    pub(crate) fn get(&self, ticket: &str) -> Option<&TicketListing> {
        self.entries.get(ticket)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{TicketAd, TicketChange, TicketListings};
    use crate::protocol::token::{self, TokenType};

    /// A structurally valid ticket of the given kind (checksum + type byte),
    /// enough for the collector's decode gate — no real address needed.
    fn ticket_of(kind: TokenType, payload: &[u8]) -> String {
        token::encode(kind, payload)
    }

    fn ad(ticket: &str, label: Option<&str>) -> String {
        TicketAd {
            ticket: ticket.to_owned(),
            label: label.map(str::to_owned),
        }
        .to_body()
        .expect("test ad body")
        .as_str()
        .to_owned()
    }

    #[test]
    fn ad_round_trips_through_body() {
        let ticket = ticket_of(TokenType::File, b"payload");
        let parsed = TicketAd::parse(&ad(&ticket, Some("chat"))).expect("parses");
        assert_eq!(parsed.ticket, ticket);
        assert_eq!(parsed.label.as_deref(), Some("chat"));
    }

    #[test]
    fn swarm_ads_and_ticket_ads_never_cross_parse() {
        // A swarm ad has `id`, a ticket ad `ticket` — each parser requires its own.
        assert!(TicketAd::parse(r#"{"id":"🐝abc","peers":2}"#).is_none());
        let ticket_ad = ad(&ticket_of(TokenType::File, b"p"), None);
        assert!(super::super::Ad::parse(&ticket_ad).is_none());
    }

    #[test]
    fn wrong_kind_tickets_are_dropped() {
        let mut dir = TicketListings::new(TokenType::Swarm);
        let file_ticket = ticket_of(TokenType::File, b"f");
        assert!(dir.note(&ad(&file_ticket, None), Instant::now()).is_none());
        assert!(dir.snapshot().is_empty());
    }

    #[test]
    fn note_found_then_updated_then_expire_lost() {
        let ticket = ticket_of(TokenType::File, b"p");
        let mut dir = TicketListings::new(TokenType::File);
        let start = Instant::now();

        assert!(matches!(
            dir.note(&ad(&ticket, Some("one")), start),
            Some(TicketChange::Found(_))
        ));
        // Unchanged re-ad: liveness refreshed, no event.
        assert!(
            dir.note(&ad(&ticket, Some("one")), start + Duration::from_secs(20))
                .is_none()
        );
        // Label change surfaces as Updated.
        assert!(matches!(
            dir.note(&ad(&ticket, Some("two")), start + Duration::from_secs(30)),
            Some(TicketChange::Updated(_))
        ));
        assert_eq!(dir.snapshot()[0].label.as_deref(), Some("two"));

        // No fresh ad past the ttl ⇒ expired/Lost.
        let lost = dir.expire(Duration::from_mins(1), start + Duration::from_secs(200));
        assert_eq!(lost, vec![ticket]);
        assert!(dir.snapshot().is_empty());
    }

    #[test]
    fn unchanged_re_ad_refreshes_liveness() {
        let ticket = ticket_of(TokenType::File, b"p");
        let mut dir = TicketListings::new(TokenType::File);
        let start = Instant::now();
        dir.note(&ad(&ticket, None), start);
        dir.note(&ad(&ticket, None), start + Duration::from_secs(50));
        assert!(
            dir.expire(Duration::from_mins(1), start + Duration::from_secs(70))
                .is_empty(),
            "the re-ad refreshed last_seen, so the entry is still alive"
        );
    }

    #[test]
    fn hostile_labels_are_truncated() {
        let ticket = ticket_of(TokenType::File, b"p");
        let long = "x".repeat(10_000);
        let mut dir = TicketListings::new(TokenType::File);
        dir.note(&ad(&ticket, Some(&long)), Instant::now());
        assert_eq!(dir.snapshot()[0].label.as_ref().map(String::len), Some(64));
    }

    #[test]
    fn password_bit_matches_the_real_ticket_encoders() {
        use iroh::{EndpointAddr, SecretKey};
        let addr = EndpointAddr::new(SecretKey::from_bytes(&[3u8; 32]).public())
            .with_ip_addr("127.0.0.1:4242".parse().unwrap());
        for password in [false, true] {
            let kind = TokenType::File;
            let ticket = crate::file::test_ticket(addr.clone(), password);
            let (decoded_kind, payload) = token::decode(&ticket).expect("decode");
            assert_eq!(decoded_kind, kind);
            assert_eq!(
                super::password_bit(kind, &payload),
                password,
                "{kind:?} password-bit offset drifted from its codec"
            );
        }
    }

    #[test]
    fn passworded_ticket_listing_carries_the_flag() {
        use iroh::{EndpointAddr, SecretKey};
        let addr = EndpointAddr::new(SecretKey::from_bytes(&[5u8; 32]).public())
            .with_ip_addr("127.0.0.1:4242".parse().unwrap());
        let ticket = crate::file::test_ticket(addr, true);
        let mut dir = TicketListings::new(TokenType::File);
        dir.note(&ad(&ticket, Some("drop")), Instant::now());
        assert!(dir.snapshot()[0].password);
    }

    #[test]
    fn junk_and_malformed_tickets_are_ignored() {
        let mut dir = TicketListings::new(TokenType::File);
        assert!(dir.note("", Instant::now()).is_none());
        assert!(dir.note("not json", Instant::now()).is_none());
        assert!(
            dir.note(r#"{"ticket":"🐝not-a-real-token"}"#, Instant::now())
                .is_none(),
            "a ticket that fails the checksum decode is dropped"
        );
        assert!(dir.snapshot().is_empty());
    }
}
