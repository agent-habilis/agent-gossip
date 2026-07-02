//! The port producer: bind an endpoint, print the consumer's `ahsw port connect`
//! command on stdout, then proxy each inbound stream to a local TCP service.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use iroh::Endpoint;
use iroh::endpoint::{Connection, Incoming, RecvStream, SendStream};
use rand::RngCore;
use tokio::net::TcpStream;

use crate::directory::ticket::TicketAd;
use crate::lookup::build_endpoint;
use crate::protocol::crypto::{Password, TicketAuth, ct_eq};
use crate::protocol::swarm::{
    DirectorySelection, LookupOpts, LookupSet, resolve_transfer_lookups, validate_advertise,
};

use super::http_log::{human_bytes, human_duration};
use super::ticket::{MAX_TICKET_PORTS, PortTicket};
use super::{PORT_ALPN, SECRET_LEN, STREAM_HEADER_LEN, wait_online};

/// Producer: expose `ports` (each on `127.0.0.1`) to peers, multiplexed over one
/// shared connection per consumer. Prints the consumer's `ahsw port connect`
/// command on stdout; serves many connections and many streams. The discovery
/// config comes from `swarm` or the create-style `flags`; `advertise`
/// re-broadcasts the ticket into a directory so a peer can find it with
/// `ahsw port discover`.
///
/// # Errors
/// Endpoint bind / discovery-config resolution failures, `--advertise` on an
/// unreachable config, or too many ports for the ticket's one-byte count field.
pub(crate) async fn listen(
    swarm: Option<&str>,
    flags: LookupSet,
    advertise: DirectorySelection,
    ports: &[u16],
    json: bool,
    password: Option<Password>,
) -> Result<()> {
    let mut deduped: Vec<u16> = Vec::with_capacity(ports.len());
    for &port in ports {
        if !deduped.contains(&port) {
            deduped.push(port);
        }
    }
    // clap's `required = true` guarantees at least one; the ticket counts
    // ports in the low 7 bits of one byte (bit 7 is the password flag), so
    // the list can't exceed 127.
    if deduped.len() > MAX_TICKET_PORTS {
        bail!("too many ports (max {MAX_TICKET_PORTS})");
    }
    let lookups = resolve_transfer_lookups(swarm, flags)?;
    validate_advertise(&advertise, &lookups)?;
    let (endpoint, ticket, auth) =
        bind(lookups.clone(), deduped.clone(), password.as_ref()).await?;
    let ports_hint = deduped
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    let _advertiser = match advertise.directory() {
        Some(directory) => {
            let ad = TicketAd {
                ticket: ticket.encode(),
                label: Some(format!("ports {ports_hint}")),
            };
            if !json {
                crate::util::output::status("Advertising", &format!("in #{directory} directory"));
            }
            Some(crate::embed::spawn_ticket_advertiser(
                directory, lookups, &ad,
            )?)
        }
        None => None,
    };
    super::announce(
        json,
        &format!("127.0.0.1 ports [{ports_hint}] → swarm"),
        &format!("ahsw port connect {} {ports_hint}", ticket.encode()),
    );
    let allowed: Arc<HashSet<u16>> = Arc::new(deduped.into_iter().collect());
    while let Some(incoming) = endpoint.accept().await {
        let allowed = Arc::clone(&allowed);
        let auth = auth.clone();
        let narrate = !json;
        tokio::spawn(async move {
            if let Err(error) = serve_connection(incoming, auth, &allowed, narrate).await {
                tracing::debug!(%error, "tcp forward connection ended");
            }
        });
    }
    // The accept loop ended (endpoint closed) — shut down gracefully.
    endpoint.close().await;
    Ok(())
}

/// Bind the producer endpoint and mint its ticket + the auth token each
/// stream header must carry (the raw secret, or its Argon2id stretch when
/// passworded) — no I/O, no print.
pub(super) async fn bind(
    lookups: LookupOpts,
    target_ports: Vec<u16>,
    password: Option<&Password>,
) -> Result<(Endpoint, PortTicket, TicketAuth)> {
    let endpoint = build_endpoint(&lookups, None, None, vec![PORT_ALPN.to_vec()]).await?;
    // Loopback needs no online wait (the bound addr is immediately usable).
    if !lookups.is_loopback() {
        wait_online(&endpoint).await;
    }
    let mut secret = [0u8; SECRET_LEN];
    rand::rng().fill_bytes(&mut secret);
    let auth = TicketAuth::derive(&secret, password);
    let ticket = PortTicket {
        addr: endpoint.addr(),
        secret,
        lookups,
        target_ports,
        password: auth.password_protected,
    };
    Ok((endpoint, ticket, auth))
}

/// Accept one inbound connection and serve each of its bi-streams as an
/// independent TCP flow — the multiplexing point. Ends when the peer closes the
/// connection (or a bad secret forces it closed from `serve_stream`).
pub(super) async fn serve_connection(
    incoming: Incoming,
    auth: TicketAuth,
    allowed: &Arc<HashSet<u16>>,
    narrate: bool,
) -> Result<()> {
    let conn = incoming.await?;
    // `accept_bi` errors once the connection is gone (peer closed, or a bad
    // token closed it from within a stream task) — that ends the loop.
    while let Ok((send, recv)) = conn.accept_bi().await {
        let conn = conn.clone();
        let allowed = Arc::clone(allowed);
        let auth = auth.clone();
        tokio::spawn(async move {
            if let Err(error) = serve_stream(&conn, send, recv, &auth, &allowed, narrate).await {
                tracing::debug!(%error, "tcp forward stream ended");
            }
        });
    }
    Ok(())
}

/// Authenticate one bi-stream by its 34-byte header, dial the named local port,
/// and proxy. A bad token closes the whole connection (the bearer is
/// poisoned) — code 3 ("wrong password") on a passworded ticket, code 1
/// ("bad secret") otherwise; a good token naming an unadvertised port resets
/// only this stream.
async fn serve_stream(
    conn: &Connection,
    send: SendStream,
    mut recv: RecvStream,
    auth: &TicketAuth,
    allowed: &HashSet<u16>,
    narrate: bool,
) -> Result<()> {
    let mut header = [0u8; STREAM_HEADER_LEN];
    if recv.read_exact(&mut header).await.is_err() {
        // The stream died before delivering a full header — nothing to serve.
        return Ok(());
    }
    let mut token = [0u8; SECRET_LEN];
    token.copy_from_slice(&header[..SECRET_LEN]);
    if !ct_eq(&token, &auth.token) {
        if auth.password_protected {
            conn.close(3u32.into(), b"wrong password");
        } else {
            conn.close(1u32.into(), b"bad secret");
        }
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
        super::proxy(tcp, send, recv, Some(port.to_string()), narrate, false).await;
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

#[cfg(test)]
mod tests {
    use super::{SECRET_LEN, bind, serve_connection};
    use crate::lookup::{add_peer_addr, build_participant_endpoint};
    use crate::port::consume::{SharedConnection, forward_one};
    use crate::protocol::swarm::LookupOpts;
    use iroh::endpoint::Connection;
    use std::collections::HashSet;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    /// A local TCP echo-ish service: read one request, reply with `response`,
    /// close. Returns the request it saw. Binds `127.0.0.1:0`, returning the
    /// port it landed on so the producer (which dials `127.0.0.1:{port}`) can be
    /// pointed at it.
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
    /// auth token, and the producer's serve task.
    async fn producer_and_consumer(
        ports: Vec<u16>,
    ) -> (
        iroh::Endpoint,
        iroh::Endpoint,
        SharedConnection,
        crate::protocol::crypto::TicketAuth,
        tokio::task::JoinHandle<()>,
    ) {
        let (producer_endpoint, ticket, auth) = bind(LookupOpts::loopback(), ports.clone(), None)
            .await
            .expect("bind producer");
        let consumer_auth = auth.clone();
        let allowed: Arc<HashSet<u16>> = Arc::new(ports.into_iter().collect());
        let producer_endpoint_for_task = producer_endpoint.clone();
        let producer_task = tokio::spawn(async move {
            let incoming = producer_endpoint_for_task
                .accept()
                .await
                .expect("inbound connection");
            serve_connection(incoming, auth, &allowed, true)
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
            consumer_auth,
            producer_task,
        )
    }

    /// Drive one local TCP client through `forward_one` on `port` and return the
    /// response it received. A fresh local listener stands in for `connect`'s
    /// per-port listener; `port` is the producer-side target the header names.
    async fn drive_client(
        shared: &SharedConnection,
        auth: &crate::protocol::crypto::TicketAuth,
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
        forward_one(shared, port, auth, accepted_tcp, peer_addr, true)
            .await
            .expect("forward_one");
        client_task.await.expect("client task")
    }

    /// Drive `serve_connection`/`forward_one` directly — the same building blocks
    /// the public CLI wrappers use (which loop forever and never expose the
    /// ephemeral port they bind, so aren't directly testable). A single port
    /// still forwards an HTTP exchange byte-for-byte, and the access-log tap
    /// doesn't alter what's forwarded.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn round_trip_forwards_an_http_exchange_byte_for_byte() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec();
        let (port, target_task) = spawn_target(response.clone()).await;
        let (producer_endpoint, consumer_endpoint, shared, auth, producer_task) =
            producer_and_consumer(vec![port]).await;

        let request = b"GET / HTTP/1.1\r\nHost: x\r\n\r\n".to_vec();
        let received_response = drive_client(&shared, &auth, port, request.clone()).await;
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

    /// Two ports, two flows — but a single shared QUIC connection. The consumer's
    /// shared connection is dialed exactly once (same `stable_id` before and
    /// after both flows — no per-flow redial), and the producer serves both off
    /// the one accepted connection.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_ports_share_one_quic_connection() {
        let (port_a, target_a) = spawn_target(b"AAA".to_vec()).await;
        let (port_b, target_b) = spawn_target(b"BBB".to_vec()).await;
        let (producer_endpoint, consumer_endpoint, shared, auth, producer_task) =
            producer_and_consumer(vec![port_a, port_b]).await;

        // Dial once up front and record which connection both flows must reuse.
        let dialed_id = shared.get().await.expect("initial dial").stable_id();

        let response_a = drive_client(&shared, &auth, port_a, b"req-a".to_vec()).await;
        let response_b = drive_client(&shared, &auth, port_b, b"req-b".to_vec()).await;

        assert_eq!(response_a, b"AAA", "port A response");
        assert_eq!(response_b, b"BBB", "port B response");
        assert_eq!(target_a.await.expect("target a"), b"req-a");
        assert_eq!(target_b.await.expect("target b"), b"req-b");

        // Both flows reused the one connection — no redial, still live.
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

    /// A passworded forward: a stream header carrying the raw ticket secret
    /// (what a directory ad leaks) kills the connection with the
    /// wrong-password close code; the derived token forwards normally.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn password_gates_the_stream_header() {
        use crate::protocol::crypto::{Password, TicketAuth};
        let response = b"HTTP/1.1 200 OK\r\n\r\nok".to_vec();
        let (port, target_task) = spawn_target(response.clone()).await;
        let password = Password::new("hunter2".to_owned());
        let (producer_endpoint, ticket, auth) =
            bind(LookupOpts::loopback(), vec![port], Some(&password))
                .await
                .expect("bind producer");
        assert!(ticket.password, "the ticket must carry the password flag");
        let allowed: Arc<HashSet<u16>> = Arc::new([port].into_iter().collect());
        let producer_endpoint_for_task = producer_endpoint.clone();
        let producer_task = tokio::spawn(async move {
            // Two connections: the raw-secret impostor, then the real consumer.
            for _ in 0..2u8 {
                let Some(incoming) = producer_endpoint_for_task.accept().await else {
                    return;
                };
                let _ = serve_connection(incoming, auth.clone(), &allowed, true).await;
            }
        });

        let consumer_endpoint = build_participant_endpoint(&ticket.lookups)
            .await
            .expect("consumer endpoint");
        add_peer_addr(&consumer_endpoint, ticket.addr.clone()).expect("add peer addr");

        // Impostor: presents the raw ticket secret in the stream header.
        let impostor = SharedConnection::new(consumer_endpoint.clone(), ticket.addr.clone());
        let conn = impostor.get().await.expect("impostor conn");
        let (mut send, mut recv) = conn.open_bi().await.expect("open stream");
        let mut header = [0u8; super::STREAM_HEADER_LEN];
        header[..SECRET_LEN].copy_from_slice(&ticket.secret);
        header[SECRET_LEN..].copy_from_slice(&port.to_be_bytes());
        send.write_all(&header).await.expect("write header");
        // The producer closes the whole connection with code 3.
        let _ = recv.read_to_end(64).await;
        let closed = conn.close_reason();
        assert!(
            matches!(
                closed,
                Some(iroh::endpoint::ConnectionError::ApplicationClosed(ref close))
                    if u64::from(close.error_code) == 3
            ),
            "raw secret must be closed with the wrong-password code, got {closed:?}"
        );
        drop(conn);

        // The real consumer's derived token forwards normally.
        let shared = SharedConnection::new(consumer_endpoint.clone(), ticket.addr.clone());
        let real_auth = TicketAuth::derive(&ticket.secret, Some(&password));
        let received = drive_client(&shared, &real_auth, port, b"req".to_vec()).await;
        assert_eq!(received, response);
        assert_eq!(target_task.await.expect("target"), b"req");

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
        let (producer_endpoint, consumer_endpoint, shared, auth, producer_task) =
            producer_and_consumer(vec![good_port]).await;

        let conn: Connection = shared.get().await.expect("shared conn");

        // Bad-port stream: valid token, unadvertised port → producer resets it.
        let (mut bad_send, mut bad_recv) = conn.open_bi().await.expect("open bad stream");
        let mut bad_header = [0u8; super::STREAM_HEADER_LEN];
        bad_header[..SECRET_LEN].copy_from_slice(&auth.token);
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
        let received = drive_client(&shared, &auth, good_port, request).await;
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
