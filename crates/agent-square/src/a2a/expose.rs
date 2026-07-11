//! The a2a exposer: bind an endpoint next to a local A2A server, print the
//! consumer's `agent-square a2a connect` command, and raw-proxy each authenticated
//! bi-stream to the origin. Strictly 1:1 — one consumer connection is served at
//! a time; a second is refused until the first disconnects.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use iroh::Endpoint;
use iroh::endpoint::{Connection, Incoming, RecvStream, SendStream};
use rand::RngCore;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

use agent_habilis_mesh::lookup::build_endpoint;
use agent_habilis_mesh::protocol::crypto::{Password, TicketAuth, ct_eq};
use agent_habilis_mesh::protocol::mesh::{
    DirectorySelection, LookupOpts, LookupSet, resolve_lookups, validate_advertise,
};

use super::directory::{TicketAd, spawn_ticket_advertiser};
use super::ticket::A2aTicket;
use super::{A2A_ALPN, SECRET_LEN, wait_online};

/// Close code for a consumer refused because the 1:1 bridge is already paired.
const BRIDGE_BUSY: u32 = 4;
/// Close code for a stream whose auth token is the raw secret on a passworded
/// ticket (what a directory ad leaks).
const WRONG_PASSWORD: u32 = 3;
/// Close code for a stream whose auth token matches no secret at all.
const BAD_SECRET: u32 = 1;

/// The a2a bridge's target + lookup/advertise/output configuration — grouped
/// (matching the embed facade's `*Config` structs) since `expose` is a single
/// CLI operation with no environment handles to keep separate.
pub(crate) struct ExposeParams<'a> {
    pub(crate) to: &'a str,
    pub(crate) flags: LookupSet,
    pub(crate) advertise: DirectorySelection,
    pub(crate) loopback: bool,
    pub(crate) password: Option<Password>,
}

/// Exposer: forward a local `http://` A2A origin to a single peer over the
/// mesh. Prints the consumer's `agent-square a2a connect` command on stdout.
///
/// `loopback` (a hidden test flag) forces loopback-only lookups so two agents
/// on one machine bridge hermetically off the ticket's direct address, with no
/// mDNS/DHT/relay.
///
/// # Errors
/// A `--to` that is not a plain `http://` URL, `--advertise` on an unreachable
/// (loopback) config, or endpoint bind / discovery-config failures.
pub(crate) async fn expose(params: ExposeParams<'_>) -> Result<()> {
    let ExposeParams {
        to,
        flags,
        advertise,
        loopback,
        password,
    } = params;
    let origin = parse_origin(to)?;
    // A bridge is a network transfer: default (no lookup flag) is the all-on
    // public preset; naming a granular flag restricts to it. `--loopback` is the
    // hermetic same-machine override.
    let lookups = if loopback {
        LookupOpts::loopback()
    } else {
        resolve_lookups(true, flags)
    };
    validate_advertise(&advertise, &lookups)?;
    let (endpoint, ticket, auth) = bind(lookups.clone(), password.as_ref()).await?;
    let _advertiser = match advertise.directory() {
        Some(directory) => {
            let ad = TicketAd {
                ticket: ticket.encode(),
                label: Some(format!("a2a {origin}")),
            };
            Some(spawn_ticket_advertiser(directory, lookups, &ad)?)
        }
        None => None,
    };
    super::announce(
        &format!("A2A {origin} → square"),
        &format!("agent-square a2a connect {}", ticket.encode()),
    );
    let origin = Arc::new(origin);
    let paired = Arc::new(AtomicBool::new(false));
    while let Some(incoming) = endpoint.accept().await {
        let origin = Arc::clone(&origin);
        let auth = auth.clone();
        let paired = Arc::clone(&paired);
        tokio::spawn(async move {
            if let Err(error) = serve_connection(incoming, &origin, auth, &paired).await {
                tracing::debug!(%error, "a2a bridge connection ended");
            }
        });
    }
    endpoint.close().await;
    Ok(())
}

/// Bind the exposer endpoint and mint its ticket + the auth token each stream
/// header must carry (the raw secret, or its Argon2id stretch when passworded).
pub(super) async fn bind(
    lookups: LookupOpts,
    password: Option<&Password>,
) -> Result<(Endpoint, A2aTicket, TicketAuth)> {
    let endpoint = build_endpoint(&lookups, None, None, vec![A2A_ALPN.to_vec()], None).await?;
    if !lookups.is_loopback() {
        wait_online(&endpoint).await;
    }
    let mut secret = [0u8; SECRET_LEN];
    rand::rng().fill_bytes(&mut secret);
    let auth = TicketAuth::a2a(&secret, password);
    let ticket = A2aTicket {
        addr: endpoint.addr(),
        secret,
        lookups,
        password: auth.password_protected,
    };
    Ok((endpoint, ticket, auth))
}

/// Serve one inbound connection, holding the 1:1 slot for its lifetime. A
/// second connection arriving while one is paired is closed immediately with
/// [`BRIDGE_BUSY`]; the slot frees when the paired connection drops so a
/// reconnect can re-pair.
pub(super) async fn serve_connection(
    incoming: Incoming,
    origin: &str,
    auth: TicketAuth,
    paired: &AtomicBool,
) -> Result<()> {
    let conn = incoming.await?;
    if paired.swap(true, Ordering::SeqCst) {
        conn.close(BRIDGE_BUSY.into(), b"bridge busy: already paired");
        tracing::info!("refused a second consumer — the 1:1 bridge is already paired");
        return Ok(());
    }
    let result = serve_streams(&conn, origin, &auth).await;
    paired.store(false, Ordering::SeqCst);
    result
}

/// Serve each bi-stream of the paired connection as an independent HTTP flow.
/// Ends when the peer closes the connection (or a bad token closes it).
async fn serve_streams(conn: &Connection, origin: &str, auth: &TicketAuth) -> Result<()> {
    while let Ok((send, recv)) = conn.accept_bi().await {
        let conn = conn.clone();
        let auth = auth.clone();
        let origin = origin.to_owned();
        tokio::spawn(async move {
            if let Err(error) =
                serve_stream(&conn, BiStreamHalves { send, recv }, &auth, &origin).await
            {
                tracing::debug!(%error, "a2a bridge stream ended");
            }
        });
    }
    Ok(())
}

/// The QUIC bi-stream's two halves — grouped so `serve_stream` stays within
/// the argument budget alongside its connection/auth/origin params.
struct BiStreamHalves {
    send: SendStream,
    recv: RecvStream,
}

/// Authenticate one bi-stream by its 32-byte token header, dial the origin, and
/// raw-proxy. A bad token closes the whole connection (the bearer is poisoned).
async fn serve_stream(
    conn: &Connection,
    streams: BiStreamHalves,
    auth: &TicketAuth,
    origin: &str,
) -> Result<()> {
    let BiStreamHalves { send, mut recv } = streams;
    let mut token = [0u8; SECRET_LEN];
    if recv.read_exact(&mut token).await.is_err() {
        return Ok(());
    }
    if !ct_eq(&token, &auth.token) {
        let code = if auth.password_protected {
            WRONG_PASSWORD
        } else {
            BAD_SECRET
        };
        conn.close(code.into(), b"bad token");
        return Ok(());
    }
    let tcp = TcpStream::connect(origin)
        .await
        .with_context(|| format!("connecting to the A2A origin {origin} failed"))?;
    raw_proxy(tcp, send, recv).await;
    Ok(())
}

/// Bidirectionally splice the origin TCP stream and the QUIC bi-stream: the
/// consumer's request arrives over QUIC and is written to the origin; the
/// origin's response is read back and written to QUIC. Bytes are untouched —
/// the exposer is fully transparent (SSE and every response stream through).
async fn raw_proxy(origin: TcpStream, mut quic_send: SendStream, mut quic_recv: RecvStream) {
    let (mut origin_read, mut origin_write) = origin.into_split();
    let request = async {
        let _ = tokio::io::copy(&mut quic_recv, &mut origin_write).await;
        let _ = origin_write.shutdown().await;
    };
    let response = async {
        let _ = tokio::io::copy(&mut origin_read, &mut quic_send).await;
        let _ = quic_send.finish();
        // Give the peer time to read the tail before the connection drops — a
        // fast/loopback link can race CONNECTION_CLOSE ahead of the last bytes.
        let _ = tokio::time::timeout(Duration::from_secs(2), quic_send.stopped()).await;
    };
    tokio::join!(request, response);
}

/// Validate and normalize `--to` into a `host:port` the exposer dials. Requires
/// a plain `http://` origin (https / path-prefixed origins are out of scope);
/// any path is dropped, since the exposer forwards raw TCP and the client's own
/// requests carry their paths.
fn parse_origin(to: &str) -> Result<String> {
    let rest = to.strip_prefix("http://").ok_or_else(|| {
        if to.starts_with("https://") {
            anyhow!(
                "https origins are out of scope — the a2a bridge forwards to a plain http:// origin"
            )
        } else {
            anyhow!("--to must be an http:// URL, e.g. http://127.0.0.1:9999")
        }
    })?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if authority.is_empty() {
        bail!("--to is missing a host, e.g. http://127.0.0.1:9999");
    }
    // A2A servers name a port; default to 80 only when one is omitted.
    if authority.contains(':') {
        Ok(authority.to_owned())
    } else {
        Ok(format!("{authority}:80"))
    }
}

#[cfg(test)]
mod tests {
    use super::parse_origin;

    #[test]
    fn parses_a_plain_http_origin() {
        assert_eq!(
            parse_origin("http://127.0.0.1:9999").unwrap(),
            "127.0.0.1:9999"
        );
    }

    #[test]
    fn drops_the_path_from_the_origin() {
        assert_eq!(
            parse_origin("http://127.0.0.1:9999/a2a/v1").unwrap(),
            "127.0.0.1:9999"
        );
    }

    #[test]
    fn defaults_the_port_when_omitted() {
        assert_eq!(
            parse_origin("http://example.test").unwrap(),
            "example.test:80"
        );
    }

    #[test]
    fn rejects_https_with_a_clear_message() {
        let error = parse_origin("https://127.0.0.1:9999")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("https origins are out of scope"),
            "got: {error}"
        );
    }

    #[test]
    fn rejects_a_non_http_scheme() {
        assert!(parse_origin("127.0.0.1:9999").is_err());
        assert!(parse_origin("tcp://127.0.0.1:9999").is_err());
    }
}
