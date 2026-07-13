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

pub(crate) const A2A_ALPN: &[u8] = b"agent-square/a2a/1";
pub(crate) const SECRET_LEN: usize = 32;

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
