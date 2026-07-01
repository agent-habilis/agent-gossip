//! The port producer: bind an endpoint, print the consumer's `ahsw port connect`
//! command on stdout, then proxy each inbound connection to a local TCP service.

use std::net::SocketAddr;
use std::time::Instant;

use anyhow::{Context, Result};
use iroh::Endpoint;
use iroh::endpoint::Incoming;
use rand::RngCore;
use tokio::net::TcpStream;

use crate::lookup::build_endpoint;
use crate::protocol::swarm::LookupOpts;

use super::http_log::{human_bytes, human_duration};
use super::ticket::PortTicket;
use super::{PORT_ALPN, SECRET_LEN, wait_online};

/// Producer: serve inbound connections by proxying each to `target` (a local
/// `host:port` TCP service). Prints the consumer's `ahsw port connect` command
/// on stdout; one ticket serves many connections.
///
/// # Errors
/// Endpoint bind / discovery-config parse failures.
pub(crate) async fn listen(swarm: Option<&str>, target: &str, json: bool) -> Result<()> {
    let lookups = super::swarm_lookups(swarm)?;
    // `target` is always `127.0.0.1:{port}` (built by the CLI layer); the port
    // travels in the ticket so the consumer can show it too.
    let target_port = target
        .parse::<SocketAddr>()
        .map(|addr| addr.port())
        .context("the local TCP target must be host:port")?;
    let (endpoint, ticket, secret) = bind(lookups, target_port).await?;
    super::announce(
        json,
        &format!("{target} → swarm:{target_port}"),
        // The consumer fills in PORT with the local port it wants to expose.
        &format!("ahsw port connect {} PORT", ticket.encode()),
    );
    let target = target.to_owned();
    while let Some(incoming) = endpoint.accept().await {
        let target = target.clone();
        let narrate = !json;
        tokio::spawn(async move {
            if let Err(error) = serve_one(incoming, &secret, &target, narrate).await {
                tracing::debug!(%error, "tcp forward connection ended");
            }
        });
    }
    // The accept loop ended (endpoint closed) — shut down gracefully.
    endpoint.close().await;
    Ok(())
}

/// Bind the producer endpoint and mint its ticket + secret — no I/O, no print.
pub(super) async fn bind(
    lookups: LookupOpts,
    target_port: u16,
) -> Result<(Endpoint, PortTicket, [u8; SECRET_LEN])> {
    let endpoint = build_endpoint(&lookups, None, None, vec![PORT_ALPN.to_vec()]).await?;
    // Loopback needs no online wait (the bound addr is immediately usable).
    if !lookups.is_loopback() {
        wait_online(&endpoint).await;
    }
    let mut secret = [0u8; SECRET_LEN];
    rand::rng().fill_bytes(&mut secret);
    let ticket = PortTicket {
        addr: endpoint.addr(),
        secret,
        lookups,
        target_port,
    };
    Ok((endpoint, ticket, secret))
}

/// Auth one inbound connection, dial the local TCP target, and proxy.
pub(super) async fn serve_one(
    incoming: Incoming,
    secret: &[u8; SECRET_LEN],
    target: &str,
    narrate: bool,
) -> Result<()> {
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
    tracing::info!("connected");
    let started = Instant::now();
    let (bytes_up, bytes_down) = super::proxy(tcp, send, recv, None, narrate, false).await;
    let elapsed = started.elapsed();
    tracing::info!(
        "disconnected ({}↑ {}↓, {})",
        human_bytes(bytes_up),
        human_bytes(bytes_down),
        human_duration(elapsed)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{bind, serve_one};
    use crate::lookup::{add_peer_addr, build_participant_endpoint};
    use crate::protocol::swarm::LookupOpts;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    /// Drive one `port listen` ↔ `port connect` exchange over loopback
    /// endpoints — `serve_one`/`forward_one` directly, the same one-shot
    /// building blocks the `connect` accept loop spawns per connection (the
    /// public `connect`/`listen` CLI wrappers loop forever and never expose
    /// the ephemeral port they bind, so aren't directly testable — same
    /// reasoning as `pipe::tests` driving `produce`/`consume` directly instead
    /// of the CLI `listen`/`connect`). Confirms the access-log tap doesn't
    /// alter what's forwarded.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn round_trip_forwards_an_http_exchange_byte_for_byte() {
        let target_listener = TcpListener::bind("127.0.0.1:0").await.expect("bind target");
        let target_addr = target_listener.local_addr().expect("target addr");
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec();
        let response_for_target = response.clone();
        let target_task = tokio::spawn(async move {
            let (mut tcp, _) = target_listener.accept().await.expect("accept target");
            let mut request = vec![0u8; 1024];
            let read = tcp.read(&mut request).await.expect("read request");
            request.truncate(read);
            tcp.write_all(&response_for_target)
                .await
                .expect("write response");
            tcp.shutdown().await.expect("shutdown target");
            request
        });

        let (producer_endpoint, ticket, secret) = bind(LookupOpts::loopback(), target_addr.port())
            .await
            .expect("bind producer");
        let target = target_addr.to_string();
        let producer_endpoint_for_task = producer_endpoint.clone();
        let producer_task = tokio::spawn(async move {
            let incoming = producer_endpoint_for_task
                .accept()
                .await
                .expect("inbound connection");
            serve_one(incoming, &secret, &target, true)
                .await
                .expect("serve_one");
        });

        let consumer_endpoint = build_participant_endpoint(&ticket.lookups)
            .await
            .expect("consumer endpoint");
        add_peer_addr(&consumer_endpoint, ticket.addr.clone()).expect("add peer addr");

        let local_listener = TcpListener::bind("127.0.0.1:0").await.expect("bind local");
        let local_addr = local_listener.local_addr().expect("local addr");
        let request = b"GET / HTTP/1.1\r\nHost: x\r\n\r\n".to_vec();
        let request_for_client = request.clone();
        let client_task = tokio::spawn(async move {
            let mut client = TcpStream::connect(local_addr).await.expect("connect local");
            client
                .write_all(&request_for_client)
                .await
                .expect("write request");
            client.shutdown().await.expect("client shutdown write");
            let mut got = Vec::new();
            client.read_to_end(&mut got).await.expect("read response");
            got
        });
        let (accepted_tcp, peer_addr) = local_listener.accept().await.expect("accept local client");

        super::super::consume::forward_one(
            &consumer_endpoint,
            ticket.addr.clone(),
            &secret,
            accepted_tcp,
            peer_addr,
            true,
        )
        .await
        .expect("forward_one");

        let received_response = client_task.await.expect("client task");
        let received_request = target_task.await.expect("target task");
        producer_task.await.expect("producer task");
        consumer_endpoint.close().await;
        producer_endpoint.close().await;

        assert_eq!(
            received_request, request,
            "request bytes must forward unchanged"
        );
        assert_eq!(
            received_response, response,
            "response bytes must forward unchanged"
        );
    }
}
