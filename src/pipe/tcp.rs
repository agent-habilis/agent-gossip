//! TCP forwarding over the pipe — expose a local TCP service to a peer.
//! `listen-tcp` proxies each inbound pipe connection to a local TCP service;
//! `connect-tcp` binds a local TCP listener and forwards each accepted
//! connection over a fresh pipe connection. One ticket serves many connections
//! (unlike the single-shot stdio pipe), so the byte flow is bidirectional.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use iroh::endpoint::{Incoming, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};

use crate::lookup::{add_peer_addr, build_participant_endpoint};

use super::http_log::{AccessLog, Direction, Tee, human_bytes, human_duration, print_log_line};
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
    let (endpoint, mut ticket, secret) = super::produce::bind(lookups).await?;
    // `target` is always `127.0.0.1:{port}` (built by the CLI layer), so this
    // never fails — the port travels in the ticket so the consumer can show
    // it too.
    let port = target.parse::<SocketAddr>().map(|addr| addr.port()).ok();
    ticket.target_port = port;
    super::announce(
        json,
        "Forwarding",
        &format!(
            "{target} → swarm{}",
            port.map(|port| format!(":{port}")).unwrap_or_default()
        ),
        // The consumer fills in PORT with the local port it wants to expose.
        &format!("ahsw pipe connect-tcp {} PORT", ticket.encode()),
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

/// Auth one inbound pipe connection, dial the local TCP target, and proxy.
async fn serve_one(
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
    let (bytes_up, bytes_down) = proxy(tcp, send, recv, None, narrate, false).await;
    let elapsed = started.elapsed();
    tracing::info!(
        "disconnected ({}↑ {}↓, {})",
        human_bytes(bytes_up),
        human_bytes(bytes_down),
        human_duration(elapsed)
    );
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
    let target_port = ticket
        .target_port
        .context("ticket has no target port — not a listen-tcp ticket")?;
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
async fn forward_one(
    endpoint: &Endpoint,
    addr: EndpointAddr,
    secret: &[u8; SECRET_LEN],
    tcp: TcpStream,
    peer_addr: SocketAddr,
    json: bool,
) -> Result<()> {
    let conn = endpoint
        .connect(addr, PIPE_ALPN)
        .await
        .map_err(|error| anyhow::anyhow!("connecting to the pipe producer failed: {error}"))?;
    let (mut send, recv) = conn.open_bi().await?;
    send.write_all(secret)
        .await
        .context("sending the ticket secret failed")?;
    let narrate = !json;
    tracing::info!(%peer_addr, "connected");
    let started = Instant::now();
    let (bytes_up, bytes_down) = proxy(tcp, send, recv, None, narrate, true).await;
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

/// Bidirectionally proxy a TCP stream and a QUIC bi-stream until both sides
/// EOF, returning the bytes copied `(tcp → quic, quic → tcp)`. Each
/// direction's reader is wrapped in [`Tee`], which only observes the bytes
/// already produced by `tokio::io::copy` — the copy itself is untouched, so
/// the byte-forwarding path stays exactly as before this feature existed.
async fn proxy(
    tcp: TcpStream,
    mut quic_send: SendStream,
    quic_recv: RecvStream,
    label: Option<String>,
    narrate: bool,
    // `connect-tcp`'s `tcp` is the local client — it sends the HTTP
    // request (`Direction::Upstream`) and the QUIC side carries the
    // response back. `listen-tcp`'s `tcp` is the connection to the local
    // *target* service — the roles are reversed: `tcp` carries the
    // response, QUIC carries the request forwarded from the consumer.
    tcp_carries_request: bool,
) -> (u64, u64) {
    let (tcp_direction, quic_direction) = if tcp_carries_request {
        (Direction::Upstream, Direction::Downstream)
    } else {
        (Direction::Downstream, Direction::Upstream)
    };
    let (tcp_read, mut tcp_write) = tcp.into_split();
    let log = Arc::new(StdMutex::new(AccessLog::new()));
    let mut tcp_read = Tee::new(
        tcp_read,
        Arc::clone(&log),
        tcp_direction,
        label.clone(),
        narrate,
    );
    let mut quic_recv = Tee::new(
        quic_recv,
        Arc::clone(&log),
        quic_direction,
        label.clone(),
        narrate,
    );
    let upstream = async {
        let bytes_up = tokio::io::copy(&mut tcp_read, &mut quic_send)
            .await
            .unwrap_or(0);
        let _ = quic_send.finish();
        // `finish()` only marks the stream done — it doesn't wait for the
        // peer to receive it. The caller drops the `Connection` right after
        // this returns, and a connection dropped before the peer has read
        // the tail of the stream can lose it outright (a fast/loopback
        // connection races the CONNECTION_CLOSE frame ahead of the last
        // stream data). Same guard `produce.rs` already uses.
        let _ = tokio::time::timeout(Duration::from_secs(2), quic_send.stopped()).await;
        bytes_up
    };
    let downstream = async {
        let bytes_down = tokio::io::copy(&mut quic_recv, &mut tcp_write)
            .await
            .unwrap_or(0);
        let _ = tcp_write.shutdown().await;
        bytes_down
    };
    let (bytes_up, bytes_down) = tokio::join!(upstream, downstream);
    if let Some(line) = log.lock().unwrap().finish() {
        print_log_line(label.as_deref(), &line, narrate);
    }
    (bytes_up, bytes_down)
}

#[cfg(test)]
mod tests {
    use super::{forward_one, serve_one};
    use crate::lookup::{add_peer_addr, build_participant_endpoint};
    use crate::protocol::swarm::LookupOpts;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    /// Drive one `listen-tcp` ↔ `connect-tcp` exchange over loopback
    /// endpoints — `serve_one`/`forward_one` directly, the same one-shot
    /// building blocks `connect_tcp`'s accept loop spawns per connection
    /// (the public `connect_tcp`/`listen_tcp` CLI wrappers loop forever and
    /// never expose the ephemeral port they bind, so aren't directly
    /// testable — same reasoning as `pipe::tests` driving `produce`/
    /// `consume` directly instead of the CLI `listen`/`connect`). Confirms
    /// the access-log tap (added in this change) doesn't alter what's
    /// forwarded.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connect_tcp_round_trip_forwards_an_http_exchange_byte_for_byte() {
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

        let (producer_endpoint, ticket, secret) =
            super::super::produce::bind(LookupOpts::loopback())
                .await
                .expect("bind producer");
        let target = target_addr.to_string();
        let producer_endpoint_for_task = producer_endpoint.clone();
        let producer_task = tokio::spawn(async move {
            let incoming = producer_endpoint_for_task
                .accept()
                .await
                .expect("inbound pipe connection");
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

        forward_one(
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
