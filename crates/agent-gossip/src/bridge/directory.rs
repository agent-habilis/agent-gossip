//! Advertising an a2a ticket into a directory and discovering it there — the
//! `--advertise` / `a2a discover` path. A directory is itself a well-known
//! mesh (derived from a name + lookups); the exposer joins it and re-broadcasts
//! a `{"ticket": "💬…", "label": "…"}` body on a timer, and a discoverer joins
//! the same directory, collects those ads, ages out silent ones, and hands the
//! chosen ticket back to `a2a connect`.
//!
//! The pure collector ([`TicketListings`]) is clock-injected and network-free
//! so it unit-tests without a mesh; the live sides ([`spawn_ticket_advertiser`]
//! / [`TicketDirectory`]) wrap it around a directory [`MeshSession`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::api::{DIRECTORY_ADVERTISER_COHOST, MeshSession};
use agent_habilis_mesh::ops::directory::directory_mesh;
use agent_habilis_mesh::protocol::{LookupOpts, LookupSet, MeshName, resolve_lookups};
use agent_habilis_mesh::protocol::{MeshId, MessageBody, Nickname};
use agent_habilis_mesh::runtime::CoHostPolicy;
use agent_habilis_mesh::runtime::tuning::{
    advertise_interval_secs, directory_expiry_secs, directory_private_for_test,
};

use super::ticket::A2aTicket;

/// Upper bound on live listings — the directory is an open public mesh, so a
/// flood of hostile ads must not grow the picker without bound.
const MAX_LISTINGS: usize = 256;

/// Longest label an ad may carry into a listing (a hostile ad must not become an
/// unbounded picker row).
const MAX_LABEL_CHARS: usize = 64;

/// A directory ad: the exposer's full `💬…` ticket plus an optional human label.
/// Serialized as a JSON object; its `ticket` key is what tells it apart from a
/// mesh ad's `id` key, so the two never cross-parse.
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
    /// The label carries a control character or the JSON exceeds the body cap.
    pub(crate) fn to_body(&self) -> Result<MessageBody> {
        Ok(MessageBody::new(serde_json::to_string(self)?)?)
    }

    fn parse(body: &str) -> Option<Self> {
        serde_json::from_str(body).ok()
    }
}

/// One live ticket entry.
#[derive(Debug, Clone)]
pub(crate) struct TicketListing {
    pub ticket: String,
    pub label: Option<String>,
    /// `true` when the ticket carries the password flag — the ad alone does not
    /// redeem it.
    pub password: bool,
    last_seen: Instant,
}

/// The change one [`TicketListings::note`] made, keyed by the ticket string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TicketChange {
    Found(String),
    Updated(String),
}

/// The pure, network-free ticket collector.
#[derive(Debug)]
pub(crate) struct TicketListings {
    entries: HashMap<String, TicketListing>,
}

impl TicketListings {
    pub(crate) fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Process one directory message body. A valid [`TicketAd`] whose ticket
    /// decodes as an a2a ticket refreshes liveness; `Found` for a new ticket,
    /// `Updated` only when the label changed, `None` for an unchanged re-ad, a
    /// non-a2a ticket, or an unparseable body.
    pub(crate) fn note(&mut self, body: &str, now: Instant) -> Option<TicketChange> {
        let ad = TicketAd::parse(body)?;
        // The structural ticket decode is the validity gate (prefix + checksum).
        let password = A2aTicket::decode(&ad.ticket).ok()?.password;
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
            },
        );
        Some(TicketChange::Found(ad.ticket))
    }

    /// Drop every listing whose last ad is older than `ttl`, returning the
    /// tickets removed.
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

    fn get(&self, ticket: &str) -> Option<&TicketListing> {
        self.entries.get(ticket)
    }

    /// The current listings, sorted by label then ticket. Test-only: the live
    /// path streams `TicketDirectoryEvent`s rather than snapshotting.
    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> Vec<TicketListing> {
        let mut listings: Vec<TicketListing> = self.entries.values().cloned().collect();
        listings.sort_by(|left, right| {
            left.label
                .cmp(&right.label)
                .then_with(|| left.ticket.cmp(&right.ticket))
        });
        listings
    }
}

/// Advertise `ad` into `directory` on a timer, reachable over `lookups`. Returns
/// the task handle; dropping it leaves the directory. A directory-join failure
/// logs and ends the task — the bridge keeps serving, just unlisted.
///
/// # Errors
/// The ad fails to serialize into a message body (a bad label).
pub(crate) fn spawn_ticket_advertiser(
    directory: MeshName,
    lookups: LookupOpts,
    ad: &TicketAd,
) -> Result<JoinHandle<()>> {
    // Validate/render once, eagerly — a bad label errors at spawn, not silently
    // every tick.
    let body = ad.to_body()?;
    Ok(tokio::spawn(async move {
        let mesh = directory_mesh(&directory, lookups);
        let session = match MeshSession::join_decoded(mesh, None, DIRECTORY_ADVERTISER_COHOST).await
        {
            Ok(session) => session,
            Err(error) => {
                tracing::warn!(
                    target: "agent_gossip::directory",
                    %error,
                    directory = %directory,
                    "a2a advertise: could not join the directory; ticket stays unlisted"
                );
                return;
            }
        };
        let mut ticker = tokio::time::interval(Duration::from_secs(advertise_interval_secs()));
        loop {
            ticker.tick().await;
            if let Err(error) = session.send(body.clone()).await {
                tracing::debug!(
                    target: "agent_gossip::directory",
                    %error,
                    "a2a advertise: re-broadcast failed (will retry next tick)"
                );
            }
        }
    }))
}

/// A ticket-directory change observed by a [`TicketDirectory`].
#[derive(Debug, Clone)]
pub(crate) enum TicketDirectoryEvent {
    Found(TicketListing),
    Updated(TicketListing),
    Lost(String),
}

/// A live view of a directory's a2a ticket ads — the consumer side of
/// [`spawn_ticket_advertiser`]. Joins as a pure consumer, collects in the
/// background, and ages out silent publishers.
#[derive(Debug)]
pub(crate) struct TicketDirectory {
    session: Option<MeshSession>,
    events_rx: Option<mpsc::UnboundedReceiver<TicketDirectoryEvent>>,
    task: Option<JoinHandle<()>>,
}

impl TicketDirectory {
    /// Open directory `name` and collect a2a ticket ads over `lookups` — the
    /// same topic-coupling rule as mesh discovery: advertiser and discoverer
    /// meet only over the same lookups.
    ///
    /// # Errors
    /// The directory session cannot be established.
    pub(crate) async fn open(name: MeshName, lookups: LookupSet) -> Result<Self> {
        let resolved = resolve_lookups(!directory_private_for_test(), lookups);
        let mesh = directory_mesh(&name, resolved);
        let session = MeshSession::join_decoded(mesh, None, CoHostPolicy::Never).await?;
        let mut inbound = session.messages();
        let listings = Arc::new(Mutex::new(TicketListings::new()));
        let (events_tx, events_rx) = mpsc::unbounded_channel();

        let collector = Arc::clone(&listings);
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
                                let mut dir = collector.lock().expect("ticket directory mutex");
                                match dir.note(message.body.as_str(), now) {
                                    Some(TicketChange::Found(ticket)) => dir
                                        .get(&ticket)
                                        .map(|listing| TicketDirectoryEvent::Found(listing.clone())),
                                    Some(TicketChange::Updated(ticket)) => dir
                                        .get(&ticket)
                                        .map(|listing| TicketDirectoryEvent::Updated(listing.clone())),
                                    None => None,
                                }
                            };
                            if let Some(event) = event {
                                let _ = events_tx.send(event);
                            }
                        }
                        // A slow consumer dropped inbound — listings self-heal on
                        // the next re-ad, so skip and continue.
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                    _ = expiry.tick() => {
                        let now = Instant::now();
                        let lost = {
                            let mut dir = collector.lock().expect("ticket directory mutex");
                            dir.expire(ttl, now)
                        };
                        for ticket in lost {
                            let _ = events_tx.send(TicketDirectoryEvent::Lost(ticket));
                        }
                    }
                }
            }
        });

        Ok(Self {
            session: Some(session),
            events_rx: Some(events_rx),
            task: Some(task),
        })
    }

    /// Take the event stream (`Found` / `Updated` / `Lost`). Single-consumer:
    /// returns the receiver once, then `None`.
    pub(crate) fn events(&mut self) -> Option<mpsc::UnboundedReceiver<TicketDirectoryEvent>> {
        self.events_rx.take()
    }

    /// The directory session's `(mesh id, nickname)` while open — for
    /// `logging::attach`.
    pub(crate) fn session_identity(&self) -> Option<(&MeshId, &Nickname)> {
        self.session
            .as_ref()
            .map(|session| (session.mesh_id(), session.nickname()))
    }

    /// Leave the directory and stop collecting.
    ///
    /// # Errors
    /// Propagates a clean-shutdown error from [`MeshSession::leave`].
    pub(crate) async fn close(mut self) -> Result<()> {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        if let Some(session) = self.session.take() {
            session.leave().await?;
        }
        Ok(())
    }
}

impl Drop for TicketDirectory {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{TicketAd, TicketChange, TicketListings};
    use crate::bridge::ticket::A2aTicket;
    use agent_habilis_mesh::protocol::LookupOpts;
    use iroh::{EndpointAddr, SecretKey};

    fn a2a_ticket(secret: u8, password: bool) -> String {
        let addr = EndpointAddr::new(SecretKey::from_bytes(&[secret; 32]).public())
            .with_ip_addr("127.0.0.1:4242".parse().unwrap());
        A2aTicket {
            addr,
            secret: [secret; 32],
            lookups: LookupOpts::loopback(),
            password,
        }
        .encode()
    }

    fn ad_body(ticket: &str, label: Option<&str>) -> String {
        TicketAd {
            ticket: ticket.to_owned(),
            label: label.map(str::to_owned),
        }
        .to_body()
        .expect("ad body")
        .as_str()
        .to_owned()
    }

    #[test]
    fn mesh_ads_and_ticket_ads_never_cross_parse() {
        // A mesh ad has `id`; a ticket ad `ticket`.
        assert!(TicketAd::parse(r#"{"id":"💬abc","peers":2}"#).is_none());
    }

    #[test]
    fn note_found_then_updated_then_expire_lost() {
        let ticket = a2a_ticket(7, false);
        let mut dir = TicketListings::new();
        let start = Instant::now();

        assert!(matches!(
            dir.note(&ad_body(&ticket, Some("one")), start),
            Some(TicketChange::Found(_))
        ));
        assert!(
            dir.note(
                &ad_body(&ticket, Some("one")),
                start + Duration::from_secs(20)
            )
            .is_none(),
            "an unchanged re-ad refreshes liveness without an event"
        );
        assert!(matches!(
            dir.note(
                &ad_body(&ticket, Some("two")),
                start + Duration::from_secs(30)
            ),
            Some(TicketChange::Updated(_))
        ));
        assert_eq!(dir.snapshot()[0].label.as_deref(), Some("two"));

        let lost = dir.expire(Duration::from_mins(1), start + Duration::from_secs(200));
        assert_eq!(lost, vec![ticket]);
        assert!(dir.snapshot().is_empty());
    }

    #[test]
    fn passworded_ticket_listing_carries_the_flag() {
        let ticket = a2a_ticket(5, true);
        let mut dir = TicketListings::new();
        dir.note(&ad_body(&ticket, Some("drop")), Instant::now());
        assert!(dir.snapshot()[0].password);
    }

    #[test]
    fn hostile_labels_are_truncated() {
        let ticket = a2a_ticket(9, false);
        let long = "x".repeat(10_000);
        let mut dir = TicketListings::new();
        dir.note(&ad_body(&ticket, Some(&long)), Instant::now());
        assert_eq!(dir.snapshot()[0].label.as_ref().map(String::len), Some(64));
    }

    #[test]
    fn junk_and_malformed_tickets_are_ignored() {
        let mut dir = TicketListings::new();
        assert!(dir.note("", Instant::now()).is_none());
        assert!(dir.note("not json", Instant::now()).is_none());
        assert!(
            dir.note(r#"{"ticket":"💬not-a-real-token"}"#, Instant::now())
                .is_none(),
            "a ticket that fails the checksum decode is dropped"
        );
        assert!(dir.snapshot().is_empty());
    }
}
