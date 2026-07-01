//! The port consumer: bind a local TCP listener and forward each accepted
//! connection over a fresh connection to the producer.

use std::net::SocketAddr;
use std::time::Instant;

use anyhow::{Context, Result};
use iroh::{Endpoint, EndpointAddr};
use tokio::net::{TcpListener, TcpStream};

use crate::lookup::{add_peer_addr, build_participant_endpoint};

use super::http_log::{human_bytes, human_duration};
use super::ticket::PortTicket;
use super::{PORT_ALPN, SECRET_LEN};

/// Consumer: bind a local TCP listener; forward each accepted connection over a
/// fresh connection to the producer.
///
/// # Errors
/// A malformed ticket, or failure to bind the local listener / accept.
pub(crate) async fn connect(ticket: &str, local_addr: &str, json: bool) -> Result<()> {
    let ticket = PortTicket::decode(ticket)?;
    let endpoint = build_participant_endpoint(&ticket.lookups).await?;
    add_peer_addr(&endpoint, ticket.addr.clone())?;
    let listener = TcpListener::bind(local_addr)
        .await
        .with_context(|| format!("binding local TCP {local_addr} failed"))?;
    // Status → stdout (stderr is errors-only); this side carries no stdout data.
    // `json` has no machine product here, so it just suppresses the status line.
    let target_port = ticket.target_port;
    tracing::info!("swarm:{target_port} → {local_addr}");
    if !json {
        println!("🐝 swarm:{target_port} → {local_addr}");
    }
    // The loop only ends on an accept error; close the endpoint before returning
    // it so iroh shuts down gracefully instead of logging a dropped-endpoint abort.
    let result: Result<()> = async {
        loop {
            let (tcp, peer_addr) = listener
                .accept()
                .await
                .context("accepting a local TCP connection failed")?;
            let endpoint = endpoint.clone();
            let addr = ticket.addr.clone();
            let secret = ticket.secret;
            tokio::spawn(async move {
                if let Err(error) =
                    forward_one(&endpoint, addr, &secret, tcp, peer_addr, json).await
                {
                    tracing::debug!(%error, "tcp forward connection ended");
                }
            });
        }
    }
    .await;
    endpoint.close().await;
    result
}

/// Dial the producer, authenticate, and proxy one local TCP connection.
pub(super) async fn forward_one(
    endpoint: &Endpoint,
    addr: EndpointAddr,
    secret: &[u8; SECRET_LEN],
    tcp: TcpStream,
    peer_addr: SocketAddr,
    json: bool,
) -> Result<()> {
    let conn = endpoint
        .connect(addr, PORT_ALPN)
        .await
        .map_err(|error| anyhow::anyhow!("connecting to the port producer failed: {error}"))?;
    let (mut send, recv) = conn.open_bi().await?;
    send.write_all(secret)
        .await
        .context("sending the ticket secret failed")?;
    let narrate = !json;
    tracing::info!(%peer_addr, "connected");
    let started = Instant::now();
    let (bytes_up, bytes_down) = super::proxy(tcp, send, recv, None, narrate, true).await;
    let elapsed = started.elapsed();
    tracing::info!(
        %peer_addr,
        "disconnected ({}↑ {}↓, {})",
        human_bytes(bytes_up),
        human_bytes(bytes_down),
        human_duration(elapsed)
    );
    Ok(())
}
