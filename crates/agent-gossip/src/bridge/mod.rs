use std::time::Duration;

use iroh::Endpoint;

mod card_rewrite;
mod connect;
mod directory;
mod expose;
mod ticket;

#[cfg(test)]
mod tests;

pub(crate) use connect::connect;
pub(crate) use directory::{TicketDirectory, TicketDirectoryEvent};
pub(crate) use expose::{ExposeParams, expose};

pub(crate) const A2A_ALPN: &[u8] = b"agent-gossip/a2a/1";
pub(crate) const SECRET_LEN: usize = 32;

/// Byte-domain for this bridge's ticket tokens, passed to
/// `TicketAuth::derive`. It is stretched into the token and so reaches the wire:
/// fixed for the life of the ticket kind, and distinct from every other kind's
/// (the engine's blob tickets use `b"blob-ticket"`) so a shared secret never
/// derives the same passworded token twice.
pub(crate) const A2A_TICKET_LABEL: &[u8] = b"a2a-ticket";

pub(crate) fn ticket_requires_password(ticket: &str) -> bool {
    ticket::A2aTicket::decode(ticket).is_ok_and(|decoded| decoded.password)
}

async fn wait_online(endpoint: &Endpoint) {
    let _ = tokio::time::timeout(Duration::from_secs(5), endpoint.online()).await;
}

fn announce(status: &str, command: &str) {
    tracing::info!("{status}");
    println!("{command}");
}
