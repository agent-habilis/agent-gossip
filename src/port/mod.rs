//! `ahsw port` — TCP forwarding over a dedicated direct QUIC connection, off
//! the gossip log. `port listen` takes a set of ports and proxies each inbound
//! stream to `127.0.0.1:{port}`; `port connect` binds a local listener per port
//! and forwards each accepted connection to the producer. All ports and all
//! flows for one producer↔consumer pair multiplex over a single shared QUIC
//! connection — each TCP flow is one bi-stream (`open_bi`/`accept_bi`),
//! self-authenticated by a 34-byte header (`secret(32) ‖ port(2, BE)`). A bad
//! secret kills the whole connection; a good secret naming a port the ticket
//! never advertised kills only that one stream. Unlike the single-shot stdio
//! [`pipe`](crate::pipe), one ticket serves many connections and the byte flow
//! is bidirectional. The ticket is a bearer capability (a random secret)
//! carrying the producer's address + the swarm's discovery config, so the
//! consumer needs nothing but the ticket.

use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::{Context, Result};
use iroh::Endpoint;
use iroh::endpoint::{RecvStream, SendStream};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

use crate::protocol::swarm::{LookupOpts, Swarm};

use http_log::{AccessLog, Direction, Tee, print_log_line};

mod consume;
mod http_log;
mod produce;
mod ticket;

pub(crate) use consume::{PortMapping, connect};
pub(crate) use produce::listen;

/// ALPN for the port protocol — a raw bidirectional QUIC stream with its own
/// protocol identity, distinct from the stdio pipe's `PIPE_ALPN`.
pub(crate) const PORT_ALPN: &[u8] = b"agent-habilis-swarm/port/1";

/// Length of the bearer-capability secret carried in a port ticket.
pub(crate) const SECRET_LEN: usize = 32;

/// Per-stream auth header: the 32-byte bearer secret followed by the 2-byte
/// (big-endian) target port. The producer reads it off each accepted bi-stream
/// to know which local service to dial — and to reject a stream whose port the
/// ticket never advertised without tearing down the shared connection.
pub(super) const STREAM_HEADER_LEN: usize = SECRET_LEN + 2;

/// Resolve a `--swarm` id to its discovery config (`None` ⇒ a public default),
/// so a port forward traverses the network the way that swarm's members do.
fn swarm_lookups(swarm: Option<&str>) -> Result<LookupOpts> {
    match swarm {
        Some(id) => Ok(id
            .parse::<Swarm>()
            .context("invalid --swarm id")?
            .lookups()
            .clone()),
        None => Ok(LookupOpts::public_preset()),
    }
}

/// Best-effort wait (≤5s) for the endpoint to publish reachable addresses, so a
/// freshly-printed ticket resolves immediately. Never blocks forever.
async fn wait_online(endpoint: &Endpoint) {
    let _ = tokio::time::timeout(Duration::from_secs(5), endpoint.online()).await;
}

/// Present the producer's status and the consumer's ready-to-run command on
/// **stdout** — the producer's product (its stdout carries no data; that flows
/// over the network), and stderr stays errors-only. Human (default) shows a bee
/// status line + the command bold-blue on a terminal (plain when piped); `json`
/// is the bare command for machines (no status/colors).
fn announce(json: bool, status: &str, command: &str) {
    tracing::info!("{status}");
    if json {
        println!("{command}");
        return;
    }
    let (open, close) = if crate::output::stdout_color() {
        (crate::output::style::BLUE, crate::output::style::RESET)
    } else {
        ("", "")
    };
    println!("🐝 {status}");
    println!("other peer can connect with: {open}{command}{close}");
}

/// Bidirectionally proxy a TCP stream and a QUIC bi-stream until both sides
/// EOF, returning the bytes copied `(tcp → quic, quic → tcp)`. Each
/// direction's reader is wrapped in [`Tee`], which only observes the bytes
/// already produced by `tokio::io::copy` — the copy itself is untouched, so
/// the byte-forwarding path stays exactly as before.
async fn proxy(
    tcp: TcpStream,
    mut quic_send: SendStream,
    quic_recv: RecvStream,
    label: Option<String>,
    narrate: bool,
    // `port connect`'s `tcp` is the local client — it sends the HTTP
    // request (`Direction::Upstream`) and the QUIC side carries the
    // response back. `port listen`'s `tcp` is the connection to the local
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
