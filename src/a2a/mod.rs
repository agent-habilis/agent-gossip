//! `ahsw a2a` — bridge an A2A (agent-to-agent) HTTP/JSON-RPC server over the
//! swarm's iroh layer, off the gossip log. `a2a expose` binds an endpoint next
//! to a local A2A server and prints a `📡…` ticket; `a2a connect` redeems the
//! ticket, binds a local `127.0.0.1:PORT`, and forwards every HTTP request to
//! the exposer, which raw-proxies it to the origin. The connect side rewrites
//! the Agent Card's absolute URLs to the local bridge address so discovery
//! resolves through the tunnel; every other response (including `message/stream`
//! SSE) streams through byte-for-byte.
//!
//! Strictly **1:1**: an exposer serves exactly one consumer at a time. The
//! single paired consumer still multiplexes many bi-streams (concurrent
//! requests + a held-open SSE stream) over its one QUIC connection — that is
//! not fan-out. A second consumer is refused until the first disconnects.

use std::time::Duration;

use iroh::Endpoint;

mod card_rewrite;
mod connect;
mod directory;
mod expose;
mod ticket;

#[cfg(test)]
mod harness_tests;

pub(crate) use connect::connect;
pub(crate) use directory::{TicketDirectory, TicketDirectoryEvent, TicketListing};
pub(crate) use expose::expose;

/// ALPN for the a2a bridge — a raw bidirectional QUIC stream with its own
/// protocol identity, distinct from the gossip protocol's `GOSSIP_ALPN`.
pub(crate) const A2A_ALPN: &[u8] = b"agent-habilis-swarm/a2a/1";

/// Length of the bearer-capability secret carried in an a2a ticket, and of the
/// auth token that opens every bi-stream (the raw secret, or its Argon2id
/// stretch when passworded — same size either way).
pub(crate) const SECRET_LEN: usize = 32;

/// Whether `ticket` decodes as a password-protected a2a ticket — the CLI's
/// prompt-before-connect check. `false` on any decode failure: the connect
/// path re-decodes and surfaces the real error.
pub(crate) fn ticket_requires_password(ticket: &str) -> bool {
    ticket::A2aTicket::decode(ticket).is_ok_and(|decoded| decoded.password)
}

/// Best-effort wait (≤5s) for the endpoint to publish reachable addresses, so a
/// freshly-printed ticket resolves immediately. Never blocks forever.
async fn wait_online(endpoint: &Endpoint) {
    let _ = tokio::time::timeout(Duration::from_secs(5), endpoint.online()).await;
}

/// Present the exposer's status and the consumer's ready-to-run command on
/// **stdout** — the exposer's product (its stdout carries no data; that flows
/// over the network), and stderr stays errors-only. Human (default) shows a
/// status line + the command in blue on a terminal (plain when piped); `json`
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
    println!("📡 {status}");
    println!("other peer can connect with: {open}{command}{close}");
}
