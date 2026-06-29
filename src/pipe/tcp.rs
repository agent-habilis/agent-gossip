//! TCP forwarding over the pipe — expose a local TCP service to a peer.
//! `listen-tcp` proxies each inbound pipe connection to a local TCP service;
//! `connect-tcp` binds a local TCP listener and forwards each accepted
//! connection over a fresh pipe connection. One ticket serves many connections
//! (unlike the single-shot stdio pipe), so the byte flow is bidirectional.

use anyhow::{Context, Result};
use iroh::endpoint::{Incoming, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};

use crate::lookup::{add_peer_addr, build_participant_endpoint};

use super::ticket::PipeTicket;
use super::{PIPE_ALPN, SECRET_LEN};

/// Producer: serve inbound pipe connections by proxying each to `target` (a
/// local `host:port` TCP service). Prints the consumer's `ahsw pipe connect-tcp`
/// command on stdout; serves many.
///
/// # Errors
/// Endpoint bind / discovery-config parse failures.
pub(crate) async fn listen_tcp(swarm: Option<&str>, target: &str, json: bool) -> Result<()> {
    let lookups = super::swarm_lookups(swarm)?;
    let (endpoint, ticket, secret) = super::produce::bind(lookups).await?;
    super::announce(
        json,
        &format!("forwarding connecting peers → {target}"),
        // The consumer fills in `--addr` with the local port it wants to expose.
        &format!("ahsw pipe connect-tcp {} --addr HOST:PORT", ticket.encode()),
    );
    let target = target.to_owned();
    while let Some(incoming) = endpoint.accept().await {
        let target = target.clone();
        tokio::spawn(async move {
            if let Err(error) = serve_one(incoming, &secret, &target).await {
                tracing::debug!(%error, "tcp forward connection ended");
            }
        });
    }
    // The accept loop ended (endpoint closed) — shut down gracefully.
    endpoint.close().await;
    Ok(())
}

/// Auth one inbound pipe connection, dial the local TCP target, and proxy.
async fn serve_one(incoming: Incoming, secret: &[u8; SECRET_LEN], target: &str) -> Result<()> {
    let conn = incoming.await?;
    let (send, mut recv) = conn.accept_bi().await?;
    let mut got = [0u8; SECRET_LEN];
    if recv.read_exact(&mut got).await.is_err() || &got != secret {
        conn.close(1u32.into(), b"bad secret");
        return Ok(());
    }
    let tcp = TcpStream::connect(target)
        .await
        .with_context(|| format!("connecting to the local TCP target {target} failed"))?;
    proxy(tcp, send, recv).await;
    Ok(())
}

/// Consumer: bind a local TCP listener; forward each accepted connection over a
/// fresh pipe connection to the producer.
///
/// # Errors
/// A malformed ticket, or failure to bind the local listener / accept.
pub(crate) async fn connect_tcp(ticket: &str, local_addr: &str, json: bool) -> Result<()> {
    let ticket = PipeTicket::decode(ticket)?;
    let endpoint = build_participant_endpoint(&ticket.lookups).await?;
    add_peer_addr(&endpoint, ticket.addr.clone())?;
    let listener = TcpListener::bind(local_addr)
        .await
        .with_context(|| format!("binding local TCP {local_addr} failed"))?;
    // Status → stdout (stderr is errors-only); this side carries no stdout data.
    // `json` has no machine product here, so it just suppresses the status line.
    tracing::info!("forwarding {local_addr} → peer");
    if !json {
        println!("🐝 forwarding {local_addr} → peer");
    }
    // The loop only ends on an accept error; close the endpoint before returning
    // it so iroh shuts down gracefully instead of logging a dropped-endpoint abort.
    let result: Result<()> = async {
        loop {
            let (tcp, _peer) = listener
                .accept()
                .await
                .context("accepting a local TCP connection failed")?;
            let endpoint = endpoint.clone();
            let addr = ticket.addr.clone();
            let secret = ticket.secret;
            tokio::spawn(async move {
                if let Err(error) = forward_one(&endpoint, addr, &secret, tcp).await {
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
async fn forward_one(
    endpoint: &Endpoint,
    addr: EndpointAddr,
    secret: &[u8; SECRET_LEN],
    tcp: TcpStream,
) -> Result<()> {
    let conn = endpoint
        .connect(addr, PIPE_ALPN)
        .await
        .map_err(|error| anyhow::anyhow!("connecting to the pipe producer failed: {error}"))?;
    let (mut send, recv) = conn.open_bi().await?;
    send.write_all(secret)
        .await
        .context("sending the ticket secret failed")?;
    proxy(tcp, send, recv).await;
    Ok(())
}

/// Bidirectionally proxy a TCP stream and a QUIC bi-stream until both sides EOF.
async fn proxy(tcp: TcpStream, mut send: SendStream, mut recv: RecvStream) {
    let (mut tcp_read, mut tcp_write) = tcp.into_split();
    let upstream = async {
        let _ = tokio::io::copy(&mut tcp_read, &mut send).await;
        let _ = send.finish();
    };
    let downstream = async {
        let _ = tokio::io::copy(&mut recv, &mut tcp_write).await;
        let _ = tcp_write.shutdown().await;
    };
    tokio::join!(upstream, downstream);
}
