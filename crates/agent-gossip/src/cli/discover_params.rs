//! `agent-gossip a2a discover`: open a named directory, collect advertised a2a
//! bridge tickets, and stream `ticket_found`/`ticket_lost` JSON for an agent.
//! The collector lives in [`crate::tunnel::TicketDirectory`].

use anyhow::Result;
use habilis_network::protocol::{DEFAULT_DIRECTORY, LookupSet, MeshName};
use habilis_network::util::logging;

use super::signal::{interrupted, sigterm_stream};
use crate::tunnel::{TicketDirectory, TicketDirectoryEvent};

/// The parsed `agent-gossip a2a discover` CLI arguments.
pub(super) struct DiscoverParams {
    pub(super) directory: Option<MeshName>,
    pub(super) lookups: LookupSet,
}

/// Run `agent-gossip a2a discover`: browse `directory` for advertised bridges,
/// streaming `ticket_found`/`ticket_lost` JSON lines until interrupted.
///
/// # Errors
/// The directory session cannot be established.
pub(super) async fn discover(params: DiscoverParams) -> Result<()> {
    let DiscoverParams { directory, lookups } = params;
    let name = directory.unwrap_or_else(|| {
        MeshName::try_new(DEFAULT_DIRECTORY).expect("the default directory name is valid")
    });
    let (discoverer, mut events) = TicketDirectory::open(&name, lookups).await?;
    // Route the directory session's logs to its per-member file so the JSON
    // stream stays clean.
    let (mesh, nickname) = discoverer.session_identity();
    logging::attach(mesh, nickname);

    let mut sigterm = sigterm_stream();
    loop {
        tokio::select! {
            () = interrupted(&mut sigterm) => break,
            received = events.recv() => match received {
                Some(change) => println!("{}", ticket_event_json(&change)),
                None => break,
            },
        }
    }
    let _ = discoverer.close().await;
    Ok(())
}

/// One ticket-directory change as a JSON line. `Found` surfaces as `ticket_found`
/// (upsert semantics); a departure is `ticket_lost`. The full
/// ticket is the machine product — strip nothing.
fn ticket_event_json(event: &TicketDirectoryEvent) -> String {
    let value = match event {
        TicketDirectoryEvent::Found(listing) => serde_json::json!({
            "event": "ticket_found",
            "ticket": listing.ticket,
            "label": listing.label,
            "password": listing.password,
        }),
        TicketDirectoryEvent::Lost(ticket) => serde_json::json!({
            "event": "ticket_lost",
            "ticket": ticket,
        }),
        TicketDirectoryEvent::EventsLost(count) => serde_json::json!({
            "event": "events_lost",
            "count": count,
        }),
    };
    value.to_string()
}
