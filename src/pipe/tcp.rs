//! TCP forwarding over the pipe — expose one or more local TCP services to a
//! peer. `listen-tcp` takes a set of ports and proxies each inbound stream to
//! `127.0.0.1:{port}`; `connect-tcp` binds a local listener per port and
//! forwards each accepted connection over the pipe. All ports and all flows for
//! one producer↔consumer pair multiplex over a single shared QUIC connection —
//! each TCP flow is one bi-stream (`open_bi`/`accept_bi`), self-authenticated by
//! a 34-byte header (`secret(32) ‖ port(2, BE)`). A bad secret kills the whole
//! connection; a good secret naming a port the ticket never advertised kills
//! only that one stream.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use iroh::endpoint::{Connection, Incoming, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::task::JoinSet;

use crate::lookup::{add_peer_addr, build_participant_endpoint};

use super::http_log::{AccessLog, Direction, Tee, human_bytes, human_duration, print_log_line};
use super::ticket::PipeTicket;
use super::{PIPE_ALPN, SECRET_LEN};

/// Per-stream auth header: the 32-byte bearer secret followed by the 2-byte
/// (big-endian) target port. The producer reads it off each accepted bi-stream
/// to know which local service to dial — and to reject a stream whose port the
/// ticket never advertised without tearing down the shared connection.
const STREAM_HEADER_LEN: usize = SECRET_LEN + 2;

/// One `connect-tcp` port mapping: bind `local` on the consumer and forward to
/// the producer's `remote` target port. A bare `PORT` arg maps a port to
/// itself; `LOCAL:REMOTE` maps across (so both sides can share a host).
#[derive(Clone, Copy, Debug)]
pub(crate) struct PortMapping {
    pub local: u16,
    pub remote: u16,
}

/// Producer: expose `ports` (each on `127.0.0.1`) to peers, multiplexed over one
/// shared connection per consumer. Prints the consumer's `ahsw pipe connect-tcp`
/// command on stdout; serves many connections and many streams.
///
/// # Errors
/// Endpoint bind / discovery-config parse failures, or too many ports for the
/// ticket's one-byte count field.
pub(crate) async fn listen_tcp(swarm: Option<&str>, ports: &[u16], json: bool) -> Result<()> {
    let mut deduped: Vec<u16> = Vec::with_capacity(ports.len());
    for &port in ports {
        if !deduped.contains(&port) {
            deduped.push(port);
        }
    }
    // clap's `required = true` guarantees at least one; the ticket counts ports
    // in a single byte, so the list can't exceed 255.
    if deduped.len() > usize::from(u8::MAX) {
        bail!("too many ports (max {})", u8::MAX);
    }
    let lookups = super::swarm_lookups(swarm)?;
    let (endpoint, mut ticket, secret) = super::produce::bind(lookups).await?;
    ticket.target_ports.clone_from(&deduped);
    let ports_hint = deduped
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    super::announce(
        json,
        &format!("127.0.0.1 ports [{ports_hint}] → swarm"),
        &format!("ahsw pipe connect-tcp {} {ports_hint}", ticket.encode()),
    );
    let allowed: Arc<HashSet<u16>> = Arc::new(deduped.into_iter().collect());
    while let Some(incoming) = endpoint.accept().await {
        let allowed = Arc::clone(&allowed);
        let narrate = !json;
        tokio::spawn(async move {
            if let Err(error) = serve_connection(incoming, secret, &allowed, narrate).await {
                tracing::debug!(%error, "tcp forward connection ended");
            }
        });
    }
    // The accept loop ended (endpoint closed) — shut down gracefully.
    endpoint.close().await;
    Ok(())
}

/// Accept one inbound pipe connection and serve each of its bi-streams as an
/// independent TCP flow — the multiplexing point. Ends when the peer closes the
/// connection (or a bad secret forces it closed from `serve_stream`).
async fn serve_connection(
    incoming: Incoming,
    secret: [u8; SECRET_LEN],
    allowed: &Arc<HashSet<u16>>,
    narrate: bool,
) -> Result<()> {
    let conn = incoming.await?;
    // `accept_bi` errors once the connection is gone (peer closed, or a bad
    // secret closed it from within a stream task) — that ends the loop.
    while let Ok((send, recv)) = conn.accept_bi().await {
        let conn = conn.clone();
        let allowed = Arc::clone(allowed);
        tokio::spawn(async move {
            if let Err(error) = serve_stream(&conn, send, recv, &secret, &allowed, narrate).await {
                tracing::debug!(%error, "tcp forward stream ended");
            }
        });
    }
    Ok(())
}

/// Authenticate one bi-stream by its 34-byte header, dial the named local port,
/// and proxy. A bad secret closes the whole connection (the bearer is
/// poisoned); a good secret naming an unadvertised port resets only this stream.
async fn serve_stream(
    conn: &Connection,
    send: SendStream,
    mut recv: RecvStream,
    secret: &[u8; SECRET_LEN],
    allowed: &HashSet<u16>,
    narrate: bool,
) -> Result<()> {
    let mut header = [0u8; STREAM_HEADER_LEN];
    if recv.read_exact(&mut header).await.is_err() {
        // The stream died before delivering a full header — nothing to serve.
        return Ok(());
    }
    if &header[..SECRET_LEN] != secret {
        conn.close(1u32.into(), b"bad secret");
        return Ok(());
    }
    let port = u16::from_be_bytes([header[SECRET_LEN], header[SECRET_LEN + 1]]);
    if !allowed.contains(&port) {
        // Reject only this stream — dropping send/recv resets it, leaving the
        // shared connection and every other port untouched.
        tracing::debug!(
            port,
            "rejecting a stream for a port the ticket never advertised"
        );
        return Ok(());
    }
    let target = format!("127.0.0.1:{port}");
    let tcp = TcpStream::connect(&target)
        .await
        .with_context(|| format!("connecting to the local TCP target {target} failed"))?;
    tracing::info!(port, "connected");
    let started = Instant::now();
    let (bytes_up, bytes_down) =
        proxy(tcp, send, recv, Some(port.to_string()), narrate, false).await;
    let elapsed = started.elapsed();
    tracing::info!(
        port,
        "disconnected ({}↑ {}↓, {})",
        human_bytes(bytes_up),
        human_bytes(bytes_down),
        human_duration(elapsed)
    );
    Ok(())
}

/// A lazily-dialed, self-healing QUIC connection shared across every port and
/// flow of one `connect-tcp` invocation. `get()` reuses the live connection and
/// redials once it has closed, so all local TCP flows multiplex over a single
/// connection instead of each dialing its own.
#[derive(Clone)]
struct SharedConnection {
    endpoint: Endpoint,
    addr: EndpointAddr,
    conn: Arc<Mutex<Option<Connection>>>,
}

impl SharedConnection {
    fn new(endpoint: Endpoint, addr: EndpointAddr) -> Self {
        Self {
            endpoint,
            addr,
            conn: Arc::new(Mutex::new(None)),
        }
    }

    /// The shared connection, dialing (or redialing after a close) as needed.
    async fn get(&self) -> Result<Connection> {
        let mut guard = self.conn.lock().await;
        if let Some(conn) = guard.as_ref() {
            // `close_reason()` is the sync liveness check — `None` while live.
            if conn.close_reason().is_none() {
                return Ok(conn.clone());
            }
        }
        let conn = self
            .endpoint
            .connect(self.addr.clone(), PIPE_ALPN)
            .await
            .map_err(|error| anyhow::anyhow!("connecting to the pipe producer failed: {error}"))?;
        *guard = Some(conn.clone());
        Ok(conn)
    }
}

/// Consumer: bind a local TCP listener per port and forward each accepted
/// connection over the shared pipe connection to the producer.
///
/// # Errors
/// A malformed ticket, a requested port the ticket never advertised, or failure
/// to bind a local listener / accept.
pub(crate) async fn connect_tcp(ticket: &str, mappings: &[PortMapping], json: bool) -> Result<()> {
    let ticket = PipeTicket::decode(ticket)?;
    if ticket.target_ports.is_empty() {
        bail!("ticket has no target ports — not a listen-tcp ticket");
    }
    for mapping in mappings {
        if !ticket.target_ports.contains(&mapping.remote) {
            bail!(
                "port {} is not advertised by this ticket (offered: {:?})",
                mapping.remote,
                ticket.target_ports
            );
        }
    }
    let endpoint = build_participant_endpoint(&ticket.lookups).await?;
    add_peer_addr(&endpoint, ticket.addr.clone())?;
    // Bind every listener up front so a taken port fails fast, before serving.
    let mut listeners = Vec::with_capacity(mappings.len());
    for &mapping in mappings {
        let local = format!("127.0.0.1:{}", mapping.local);
        let listener = TcpListener::bind(&local)
            .await
            .with_context(|| format!("binding local TCP {local} failed"))?;
        // Status → stdout (stderr is errors-only); this side carries no stdout
        // data. `json` has no machine product here, so it just suppresses the line.
        tracing::info!("swarm:{} → {local}", mapping.remote);
        if !json {
            println!("🐝 swarm:{} → {local}", mapping.remote);
        }
        listeners.push((mapping.remote, listener));
    }
    let shared = SharedConnection::new(endpoint.clone(), ticket.addr.clone());
    let secret = ticket.secret;
    // One accept loop per port; each ends only on its listener's accept error.
    // The endpoint is closed before returning so iroh tears down gracefully
    // instead of logging a dropped-endpoint abort. Each loop forwards to the
    // producer's `remote` port (what travels in the stream header).
    let mut tasks: JoinSet<Result<()>> = JoinSet::new();
    for (remote, listener) in listeners {
        let shared = shared.clone();
        tasks.spawn(async move { accept_loop(shared, remote, secret, listener, json).await });
    }
    let result: Result<()> = async {
        while let Some(joined) = tasks.join_next().await {
            joined.context("a forward task panicked")??;
        }
        Ok(())
    }
    .await;
    endpoint.close().await;
    result
}

/// Accept local TCP connections on one port and forward each over the shared
/// connection. Ends only on an accept error.
async fn accept_loop(
    shared: SharedConnection,
    port: u16,
    secret: [u8; SECRET_LEN],
    listener: TcpListener,
    json: bool,
) -> Result<()> {
    loop {
        let (tcp, peer_addr) = listener
            .accept()
            .await
            .with_context(|| format!("accepting a local TCP connection on port {port} failed"))?;
        let shared = shared.clone();
        tokio::spawn(async move {
            if let Err(error) = forward_one(&shared, port, &secret, tcp, peer_addr, json).await {
                tracing::debug!(%error, port, "tcp forward connection ended");
            }
        });
    }
}

/// Open one bi-stream on the shared connection, send the 34-byte header, and
/// proxy one local TCP connection over it.
async fn forward_one(
    shared: &SharedConnection,
    port: u16,
    secret: &[u8; SECRET_LEN],
    tcp: TcpStream,
    peer_addr: SocketAddr,
    json: bool,
) -> Result<()> {
    let conn = shared.get().await?;
    let (mut send, recv) = conn.open_bi().await?;
    let mut header = [0u8; STREAM_HEADER_LEN];
    header[..SECRET_LEN].copy_from_slice(secret);
    header[SECRET_LEN..].copy_from_slice(&port.to_be_bytes());
    send.write_all(&header)
        .await
        .context("sending the stream header failed")?;
    let narrate = !json;
    tracing::info!(%peer_addr, port, "connected");
    let started = Instant::now();
    let (bytes_up, bytes_down) =
        proxy(tcp, send, recv, Some(port.to_string()), narrate, true).await;
    let elapsed = started.elapsed();
    tracing::info!(
        %peer_addr,
        port,
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
    use super::{
        Connection, SECRET_LEN, STREAM_HEADER_LEN, SharedConnection, forward_one, serve_connection,
    };
    use crate::lookup::{add_peer_addr, build_participant_endpoint};
    use crate::protocol::swarm::LookupOpts;
    use std::collections::HashSet;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    /// A local TCP echo-ish service: read one request, reply with `response`,
    /// close. Returns the request it saw. Binds `127.0.0.1:0`, returning the
    /// port it landed on so the producer (which dials `127.0.0.1:{port}`) can
    /// be pointed at it.
    async fn spawn_target(response: Vec<u8>) -> (u16, tokio::task::JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind target");
        let port = listener.local_addr().expect("target addr").port();
        let handle = tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.expect("accept target");
            let mut request = vec![0u8; 1024];
            let read = tcp.read(&mut request).await.expect("read request");
            request.truncate(read);
            tcp.write_all(&response).await.expect("write response");
            tcp.shutdown().await.expect("shutdown target");
            request
        });
        (port, handle)
    }

    /// Stand up a loopback producer serving `ports` and a consumer connected to
    /// it, returning both endpoints, the consumer's `SharedConnection`, the
    /// bearer secret, and the producer's serve task.
    async fn producer_and_consumer(
        ports: Vec<u16>,
    ) -> (
        iroh::Endpoint,
        iroh::Endpoint,
        SharedConnection,
        [u8; SECRET_LEN],
        tokio::task::JoinHandle<()>,
    ) {
        let (producer_endpoint, mut ticket, secret) =
            super::super::produce::bind(LookupOpts::loopback())
                .await
                .expect("bind producer");
        ticket.target_ports.clone_from(&ports);
        let allowed: Arc<HashSet<u16>> = Arc::new(ports.into_iter().collect());
        let producer_endpoint_for_task = producer_endpoint.clone();
        let producer_task = tokio::spawn(async move {
            let incoming = producer_endpoint_for_task
                .accept()
                .await
                .expect("inbound pipe connection");
            serve_connection(incoming, secret, &allowed, true)
                .await
                .expect("serve_connection");
        });

        let consumer_endpoint = build_participant_endpoint(&ticket.lookups)
            .await
            .expect("consumer endpoint");
        add_peer_addr(&consumer_endpoint, ticket.addr.clone()).expect("add peer addr");
        let shared = SharedConnection::new(consumer_endpoint.clone(), ticket.addr.clone());
        (
            producer_endpoint,
            consumer_endpoint,
            shared,
            secret,
            producer_task,
        )
    }

    /// Drive one local TCP client through `forward_one` on `port` and return the
    /// response it received. `local_listener` stands in for `connect-tcp`'s
    /// per-port listener; `port` is the producer-side target the header names.
    async fn drive_client(
        shared: &SharedConnection,
        secret: &[u8; SECRET_LEN],
        port: u16,
        request: Vec<u8>,
    ) -> Vec<u8> {
        let local_listener = TcpListener::bind("127.0.0.1:0").await.expect("bind local");
        let local_addr = local_listener.local_addr().expect("local addr");
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
        forward_one(shared, port, secret, accepted_tcp, peer_addr, true)
            .await
            .expect("forward_one");
        client_task.await.expect("client task")
    }

    /// Drive `serve_connection`/`forward_one` directly — the same building
    /// blocks the public CLI wrappers use (which loop forever and never expose
    /// the ephemeral port they bind, so aren't directly testable — same
    /// reasoning as `pipe::tests` driving `produce`/`consume` directly). A
    /// single port still forwards an HTTP exchange byte-for-byte, and the
    /// access-log tap doesn't alter what's forwarded.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connect_tcp_round_trip_forwards_an_http_exchange_byte_for_byte() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec();
        let (port, target_task) = spawn_target(response.clone()).await;
        let (producer_endpoint, consumer_endpoint, shared, secret, producer_task) =
            producer_and_consumer(vec![port]).await;

        let request = b"GET / HTTP/1.1\r\nHost: x\r\n\r\n".to_vec();
        let received_response = drive_client(&shared, &secret, port, request.clone()).await;
        let received_request = target_task.await.expect("target task");

        // Close the consumer first so the producer's `accept_bi` loop ends and
        // `serve_connection` returns — otherwise awaiting it would hang.
        consumer_endpoint.close().await;
        producer_task.await.expect("producer task");
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

    /// Two ports, two flows — but a single shared QUIC connection. The producer
    /// serves both flows off one accepted connection, and the consumer's shared
    /// connection is dialed exactly once (same `stable_id` before and after both
    /// flows — no per-flow redial).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_ports_share_one_quic_connection() {
        let (port_a, target_a) = spawn_target(b"AAA".to_vec()).await;
        let (port_b, target_b) = spawn_target(b"BBB".to_vec()).await;
        let (producer_endpoint, consumer_endpoint, shared, secret, producer_task) =
            producer_and_consumer(vec![port_a, port_b]).await;

        // Dial once up front and record which connection both flows must reuse.
        let dialed_id = shared.get().await.expect("initial dial").stable_id();

        let response_a = drive_client(&shared, &secret, port_a, b"req-a".to_vec()).await;
        let response_b = drive_client(&shared, &secret, port_b, b"req-b".to_vec()).await;

        assert_eq!(response_a, b"AAA", "port A response");
        assert_eq!(response_b, b"BBB", "port B response");
        assert_eq!(target_a.await.expect("target a"), b"req-a");
        assert_eq!(target_b.await.expect("target b"), b"req-b");

        // Both flows reused the one connection — no redial, still live. The
        // producer only ever accepted this single inbound connection (its
        // `serve_connection` handles exactly one), so both streams multiplexed
        // over it.
        let conn = shared.get().await.expect("shared conn");
        assert_eq!(
            conn.stable_id(),
            dialed_id,
            "both ports must reuse one connection, not redial per flow"
        );
        assert!(
            conn.close_reason().is_none(),
            "shared connection stays live"
        );
        drop(conn);

        consumer_endpoint.close().await;
        producer_task.await.expect("producer task");
        producer_endpoint.close().await;
    }

    /// A stream whose header names a port the ticket never advertised is reset,
    /// while the shared connection (and an advertised port) keeps working.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_port_not_in_the_ticket_is_rejected_without_killing_the_connection() {
        let response = b"HTTP/1.1 200 OK\r\n\r\nok".to_vec();
        let (good_port, target_task) = spawn_target(response.clone()).await;
        // Advertise only `good_port`; `bad_port` is a plausible-but-unoffered one.
        let bad_port = good_port.wrapping_add(1).max(1);
        let (producer_endpoint, consumer_endpoint, shared, secret, producer_task) =
            producer_and_consumer(vec![good_port]).await;

        let conn: Connection = shared.get().await.expect("shared conn");

        // Bad-port stream: valid secret, unadvertised port → producer resets it.
        let (mut bad_send, mut bad_recv) = conn.open_bi().await.expect("open bad stream");
        let mut bad_header = [0u8; STREAM_HEADER_LEN];
        bad_header[..SECRET_LEN].copy_from_slice(&secret);
        bad_header[SECRET_LEN..].copy_from_slice(&bad_port.to_be_bytes());
        bad_send
            .write_all(&bad_header)
            .await
            .expect("write bad header");
        let _ = bad_send.finish();
        // The producer drops the stream; the consumer sees a reset (or clean EOF
        // with no payload). Either way it gets no proxied bytes.
        let bad_read = bad_recv.read_to_end(64).await;
        assert!(
            bad_read.map_or(true, |bytes| bytes.is_empty()),
            "a rejected stream forwards no bytes"
        );

        // The connection survived: a good-port flow over the SAME connection works.
        assert!(
            conn.close_reason().is_none(),
            "connection survives a rejected stream"
        );
        let request = b"GET / HTTP/1.1\r\nHost: x\r\n\r\n".to_vec();
        let received = drive_client(&shared, &secret, good_port, request).await;
        assert_eq!(received, response, "the advertised port still forwards");
        assert_eq!(
            target_task.await.expect("target task"),
            b"GET / HTTP/1.1\r\nHost: x\r\n\r\n"
        );
        drop(conn);

        consumer_endpoint.close().await;
        producer_task.await.expect("producer task");
        producer_endpoint.close().await;
    }
}
