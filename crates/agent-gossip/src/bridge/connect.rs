//! The a2a consumer: bind a local `127.0.0.1:PORT` an unmodified A2A client
//! points at, and forward each accepted connection over one shared QUIC
//! connection to the exposer — rewriting the Agent Card's URLs on the way back.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use fofoca::iroh::endpoint::{Connection, RecvStream, SendStream};
use fofoca::iroh::{Endpoint, EndpointAddr};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

use fofoca::net::{add_peer_addr, build_peer_endpoint};
use fofoca::protocol::{Password, TicketAuth};

use super::card_rewrite::CardRewriter;
use super::ticket::A2aTicket;
use super::{A2A_ALPN, A2A_TICKET_LABEL};

/// How long the first dial keeps retrying while the exposer's address
/// propagates through the lookups.
const DISCOVERY_DEADLINE: Duration = Duration::from_secs(90);
const RETRY_DELAY: Duration = Duration::from_secs(3);

/// Consumer: redeem `ticket`, bind a local A2A endpoint, and bridge it to the
/// exposer. `local_port` is the port to bind (ephemeral when `None`).
///
/// # Errors
/// A malformed ticket, a password mismatch with the ticket, or failure to bind
/// the local listener / accept.
pub(crate) async fn connect(
    ticket: &str,
    local_port: Option<u16>,
    password: Option<Password>,
) -> Result<()> {
    let ticket = A2aTicket::decode(ticket)?;
    let auth = match (&password, ticket.password) {
        (None, true) => bail!("this ticket is password-protected — pass --password"),
        (Some(_), false) => bail!("this ticket has no password — drop --password"),
        _ => TicketAuth::derive(&ticket.secret, password.as_ref(), A2A_TICKET_LABEL),
    };
    let endpoint = build_peer_endpoint(&ticket.lookups).await?;
    add_peer_addr(&endpoint, ticket.addr.clone())?;
    let bind_addr = format!("127.0.0.1:{}", local_port.unwrap_or(0));
    let listener = TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("binding the local A2A port {bind_addr} failed"))?;
    let port = listener.local_addr()?.port();
    let local_base = format!("http://127.0.0.1:{port}");
    tracing::info!("a2a bridge listening on {local_base}");
    println!("{local_base}");
    let shared = SharedConnection::new(endpoint.clone(), ticket.addr.clone());
    let auth = Arc::new(auth);
    let local_base = Arc::new(local_base);
    let result = accept_loop(&shared, &auth, &local_base, &listener).await;
    endpoint.close().await;
    result
}

async fn accept_loop(
    shared: &SharedConnection,
    auth: &Arc<TicketAuth>,
    local_base: &Arc<String>,
    listener: &TcpListener,
) -> Result<()> {
    loop {
        let (tcp, peer) = listener
            .accept()
            .await
            .context("accepting a local A2A connection failed")?;
        let shared = shared.clone();
        let auth = Arc::clone(auth);
        let local_base = Arc::clone(local_base);
        tokio::spawn(async move {
            if let Err(error) = forward_one(&shared, &auth, tcp, &local_base).await {
                tracing::debug!(%error, %peer, "a2a bridge connection ended");
            }
        });
    }
}

/// Open one bi-stream on the shared connection, present the auth token, and
/// proxy one local client connection over it (rewriting the response).
pub(super) async fn forward_one(
    shared: &SharedConnection,
    auth: &TicketAuth,
    tcp: TcpStream,
    local_base: &str,
) -> Result<()> {
    let conn = shared.get().await?;
    let (mut send, recv) = conn.open_bi().await?;
    send.write_all(&auth.token)
        .await
        .context("sending the auth token failed")?;
    rewriting_proxy(tcp, send, recv, local_base).await;
    Ok(())
}

/// Splice the local client and the QUIC bi-stream: the client's request is
/// written to QUIC untouched; the response is streamed back through the
/// [`CardRewriter`], which rewrites only Agent Card URLs and passes everything
/// else (SSE, task results) byte-for-byte.
async fn rewriting_proxy(
    client: TcpStream,
    mut quic_send: SendStream,
    quic_recv: RecvStream,
    local_base: &str,
) {
    let (mut client_read, mut client_write) = client.into_split();
    let request = async {
        let _ = tokio::io::copy(&mut client_read, &mut quic_send).await;
        let _ = quic_send.finish();
        let _ = tokio::time::timeout(Duration::from_secs(2), quic_send.stopped()).await;
    };
    let response = rewrite_response(quic_recv, &mut client_write, local_base);
    tokio::join!(request, response);
    let _ = client_write.shutdown().await;
}

/// Read the response side of the bi-stream, feed it through the rewriter, and
/// write the (possibly rewritten) bytes to the client until EOF.
async fn rewrite_response<W>(mut quic_recv: RecvStream, client_write: &mut W, local_base: &str)
where
    W: AsyncWriteExt + Unpin,
{
    let mut rewriter = CardRewriter::new(local_base.to_owned());
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match quic_recv.read(&mut buf).await {
            // `Ok(None)` is a clean end of stream; a read error ends it too.
            Ok(None) | Err(_) => break,
            Ok(Some(read)) => {
                let out = rewriter.feed(&buf[..read]);
                if !out.is_empty() && client_write.write_all(&out).await.is_err() {
                    return;
                }
            }
        }
    }
    let tail = rewriter.finish();
    if !tail.is_empty() {
        let _ = client_write.write_all(&tail).await;
    }
}

/// A lazily-dialed, self-healing QUIC connection shared across every local flow
/// of one `a2a connect` invocation, so all HTTP connections multiplex over a
/// single QUIC connection (the 1:1 pairing the exposer enforces).
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
    /// The dial retries until [`DISCOVERY_DEADLINE`] so an exposer whose address
    /// is still propagating is waited for, not failed on.
    pub(super) async fn get(&self) -> Result<Connection> {
        let mut guard = self.conn.lock().await;
        if let Some(conn) = guard.as_ref()
            && conn.close_reason().is_none()
        {
            return Ok(conn.clone());
        }
        let started = Instant::now();
        let conn = loop {
            match self.endpoint.connect(self.addr.clone(), A2A_ALPN).await {
                Ok(conn) => break conn,
                Err(error) if started.elapsed() < DISCOVERY_DEADLINE => {
                    tracing::debug!(%error, "a2a dial retrying while the address propagates");
                    tokio::time::sleep(RETRY_DELAY).await;
                }
                Err(error) => bail!("connecting to the a2a exposer failed: {error}"),
            }
        };
        *guard = Some(conn.clone());
        Ok(conn)
    }
}
