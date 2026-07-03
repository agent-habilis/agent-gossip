//! The port consumer: bind a local TCP listener per port and forward each
//! accepted connection over the shared connection to the producer.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::task::JoinSet;

use crate::lookup::{add_peer_addr, build_participant_endpoint};
use crate::protocol::crypto::{Password, TicketAuth};

use super::http_log::{human_bytes, human_duration};
use super::ticket::PortTicket;
use super::{PORT_ALPN, SECRET_LEN, STREAM_HEADER_LEN};

/// One `port connect` port mapping: bind `local` on the consumer and forward to
/// the producer's `remote` target port. A bare `PORT` arg maps a port to
/// itself; `LOCAL:REMOTE` maps across (so both sides can share a host).
#[derive(Clone, Copy, Debug)]
pub(crate) struct PortMapping {
    pub local: u16,
    pub remote: u16,
}

/// How long the first dial keeps retrying while the producer's address
/// propagates through the lookups (mirrors `file`'s consumer, which waits
/// the same window — a `port connect` should not fail on the first flow
/// where a `file get` with the same lookups would have waited).
const DISCOVERY_DEADLINE: Duration = Duration::from_secs(90);
const RETRY_DELAY: Duration = Duration::from_secs(3);

/// A lazily-dialed, self-healing QUIC connection shared across every port and
/// flow of one `port connect` invocation. `get()` reuses the live connection
/// and redials once it has closed, so all local TCP flows multiplex over a
/// single connection instead of each dialing its own.
#[derive(Clone)]
pub(super) struct SharedConnection {
    endpoint: Endpoint,
    addr: EndpointAddr,
    conn: Arc<Mutex<Option<Connection>>>,
}

impl SharedConnection {
    pub(super) fn new(endpoint: Endpoint, addr: EndpointAddr) -> Self {
        Self {
            endpoint,
            addr,
            conn: Arc::new(Mutex::new(None)),
        }
    }

    /// The shared connection, dialing (or redialing after a close) as needed.
    /// The dial retries until [`DISCOVERY_DEADLINE`] so a producer whose address
    /// is still propagating through the lookups is waited for, not failed on.
    pub(super) async fn get(&self) -> Result<Connection> {
        let mut guard = self.conn.lock().await;
        if let Some(conn) = guard.as_ref() {
            // `close_reason()` is the sync liveness check — `None` while live.
            if conn.close_reason().is_none() {
                return Ok(conn.clone());
            }
        }
        let start = Instant::now();
        let conn = loop {
            match self.endpoint.connect(self.addr.clone(), PORT_ALPN).await {
                Ok(conn) => break conn,
                Err(error) if start.elapsed() < DISCOVERY_DEADLINE => {
                    tokio::time::sleep(RETRY_DELAY).await;
                    let _ = error;
                }
                Err(error) => {
                    return Err(anyhow::anyhow!(
                        "connecting to the port producer failed: {error}"
                    ));
                }
            }
        };
        *guard = Some(conn.clone());
        Ok(conn)
    }
}

/// Consumer: bind a local TCP listener per port and forward each accepted
/// connection over the shared connection to the producer.
///
/// # Errors
/// A malformed ticket, a password mismatch with the ticket (missing on a
/// passworded ticket, or given for a passwordless one), a requested port the
/// ticket never advertised, or failure to bind a local listener / accept.
pub(crate) async fn connect(
    ticket: &str,
    mappings: &[PortMapping],
    json: bool,
    password: Option<Password>,
) -> Result<()> {
    let ticket = PortTicket::decode(ticket)?;
    let auth = match (&password, ticket.password) {
        (None, true) => bail!("this ticket is password-protected — pass --password"),
        (Some(_), false) => bail!("this ticket has no password — drop --password"),
        _ => TicketAuth::derive(&ticket.secret, password.as_ref()),
    };
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
    // One accept loop per port; each ends only on its listener's accept error.
    // The endpoint is closed before returning so iroh tears down gracefully
    // instead of logging a dropped-endpoint abort. Each loop forwards to the
    // producer's `remote` port (what travels in the stream header).
    let mut tasks: JoinSet<Result<()>> = JoinSet::new();
    for (remote, listener) in listeners {
        let shared = shared.clone();
        let auth = auth.clone();
        tasks.spawn(async move { accept_loop(shared, remote, auth, listener, json).await });
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
    auth: TicketAuth,
    listener: TcpListener,
    json: bool,
) -> Result<()> {
    loop {
        let (tcp, peer_addr) = listener
            .accept()
            .await
            .with_context(|| format!("accepting a local TCP connection on port {port} failed"))?;
        let shared = shared.clone();
        let auth = auth.clone();
        tokio::spawn(async move {
            if let Err(error) = forward_one(&shared, port, &auth, tcp, peer_addr, json).await {
                tracing::debug!(%error, port, "tcp forward connection ended");
            }
        });
    }
}

/// Open one bi-stream on the shared connection, send the 34-byte header, and
/// proxy one local TCP connection over it.
pub(super) async fn forward_one(
    shared: &SharedConnection,
    port: u16,
    auth: &TicketAuth,
    tcp: TcpStream,
    peer_addr: SocketAddr,
    json: bool,
) -> Result<()> {
    let conn = shared.get().await?;
    let (mut send, recv) = conn.open_bi().await?;
    let mut header = [0u8; STREAM_HEADER_LEN];
    header[..SECRET_LEN].copy_from_slice(&auth.token);
    header[SECRET_LEN..].copy_from_slice(&port.to_be_bytes());
    send.write_all(&header)
        .await
        .context("sending the stream header failed")?;
    let narrate = !json;
    tracing::info!(%peer_addr, port, "connected");
    let started = Instant::now();
    let (bytes_up, bytes_down) =
        super::proxy(tcp, send, recv, Some(port.to_string()), narrate, true).await;
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
