//! The pipe consumer: redeem a ticket, dial the producer, stream to a sink.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use iroh::Endpoint;
use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::lookup::{add_peer_addr, build_participant_endpoint};

use super::PIPE_ALPN;
use super::progress::{Progress, recv_with_reports};
use super::ticket::PipeTicket;

/// How long to keep retrying the dial while the producer's address propagates
/// (mDNS is instant on a LAN; the DHT fallback can take tens of seconds).
const DISCOVERY_DEADLINE: Duration = Duration::from_secs(90);
const RETRY_DELAY: Duration = Duration::from_secs(3);

/// Redeem `ticket` and stream the producer's bytes to stdout.
///
/// # Errors
/// A malformed ticket, an unreachable producer, or a truncated transfer (the
/// producer vanished mid-stream) — the caller then exits non-zero.
pub(crate) async fn connect(ticket: &str, throttle: Option<u64>) -> Result<()> {
    let ticket = PipeTicket::decode(ticket)?;
    let endpoint = build_participant_endpoint(&ticket.lookups).await?;
    let mut stdout = tokio::io::stdout();
    match transfer(&endpoint, &ticket, &mut stdout, throttle).await {
        Ok(()) => {
            stdout.flush().await.context("flushing stdout failed")?;
            // The data is delivered and `conn.close` already told the producer
            // we're done. Exit now rather than awaiting `endpoint.close()` — that
            // tears down relay/DHT/mDNS and lingers for seconds. `process::exit`
            // skips both the teardown and the endpoint's abort-logging `Drop`.
            std::process::exit(0);
        }
        // On error, shut the endpoint down gracefully and surface the failure.
        Err(error) => {
            endpoint.close().await;
            Err(error)
        }
    }
}

/// Dial the producer over `endpoint`, authenticate, and stream to `writer`.
pub(crate) async fn transfer<W: AsyncWrite + Unpin>(
    endpoint: &Endpoint,
    ticket: &PipeTicket,
    writer: &mut W,
    throttle: Option<u64>,
) -> Result<()> {
    add_peer_addr(endpoint, ticket.addr.clone())?;

    let start = Instant::now();
    let conn = loop {
        match endpoint.connect(ticket.addr.clone(), PIPE_ALPN).await {
            Ok(conn) => break conn,
            Err(error) if start.elapsed() < DISCOVERY_DEADLINE => {
                tracing::warn!(%error, "connect failed; retrying");
                tokio::time::sleep(RETRY_DELAY).await;
            }
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "could not reach the pipe producer: {error}"
                ));
            }
        }
    };

    let (mut send, mut recv) = conn.open_bi().await.context("opening the stream failed")?;
    // Authenticate with the bearer secret; keep `send` open to report progress
    // back so the producer's bar reflects what we've actually received.
    send.write_all(&ticket.secret)
        .await
        .context("sending the ticket secret failed")?;

    // The producer sends an 8-byte length header (u64::MAX = unknown) first.
    let mut header = [0u8; 8];
    recv.read_exact(&mut header)
        .await
        .context("reading the length header failed")?;
    let len = u64::from_le_bytes(header);
    let total = (len != u64::MAX).then_some(len);

    let mut progress = Progress::new(total);
    recv_with_reports(&mut recv, writer, &mut send, &mut progress, throttle, total)
        .await
        .context("streaming to the sink failed")?;
    send.finish()
        .context("finishing the report stream failed")?;
    // Wait until the producer has acknowledged the report stream (incl. its FIN)
    // before returning — `connect` then `process::exit`s, which would otherwise
    // drop the FIN in flight and hang the producer's read loop.
    let _ = send.stopped().await;
    conn.close(0u32.into(), b"done");
    Ok(())
}
