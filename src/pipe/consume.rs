//! The pipe consumer: redeem a ticket, dial the producer, stream to a sink.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use iroh::Endpoint;
use iroh::endpoint::{Connection, ConnectionError, RecvStream, SendStream};
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::lookup::{add_peer_addr, build_participant_endpoint};

use super::PIPE_ALPN;
use super::progress::{Progress, pace, recv_with_reports, throttle_chunk};
use super::ticket::PipeTicket;

/// How long to keep retrying the dial while the producer's address propagates
/// (mDNS is instant on a LAN; the DHT fallback can take tens of seconds).
const DISCOVERY_DEADLINE: Duration = Duration::from_secs(90);
const RETRY_DELAY: Duration = Duration::from_secs(3);

/// Redeem `ticket` and stream the producer's bytes to stdout.
///
/// # Errors
/// A malformed or bench ticket, an unreachable producer, or a truncated
/// transfer (the producer vanished mid-stream) — the caller then exits
/// non-zero. A bench ticket is rejected up front: past the handshake, its
/// producer speaks the bench plan-header protocol instead of sending the
/// length header this path expects next, which would otherwise hang both
/// sides waiting on each other.
pub(crate) async fn connect(ticket: &str, throttle: Option<u64>) -> Result<()> {
    let ticket = PipeTicket::decode(ticket)?;
    if ticket.bench {
        bail!(
            "this ticket is for `pipe bench`, not `pipe connect` — \
             run `pipe listen` on the producer to get a plain pipe ticket"
        );
    }
    let endpoint = build_participant_endpoint(&ticket.lookups).await?;
    let mut stdout = tokio::io::stdout();
    let result = if ticket.follow {
        transfer_follow(&endpoint, &ticket, &mut stdout, throttle).await
    } else {
        transfer(&endpoint, &ticket, &mut stdout, throttle).await
    };
    match result {
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

/// Dial the producer (retrying while its address propagates), open the
/// bi-stream, and present the bearer secret. Shared by every consumer
/// protocol — single-shot and live-follow read the length header next
/// (below); bench (`pipe bench 🐝…`) sends its own plan header instead.
pub(super) async fn dial_and_authenticate(
    endpoint: &Endpoint,
    ticket: &PipeTicket,
) -> Result<(Connection, SendStream, RecvStream)> {
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

    let (mut send, recv) = conn.open_bi().await.context("opening the stream failed")?;
    send.write_all(&ticket.secret)
        .await
        .context("sending the ticket secret failed")?;
    Ok((conn, send, recv))
}

/// [`dial_and_authenticate`] plus the 8-byte length header the producer sends
/// next (`u64::MAX` = unknown/live) — shared by the single-shot and
/// live-follow consumers so their dial + handshake stay in lock-step. Returns
/// the connection, both streams, and the advertised length (`None` = unknown
/// / live).
async fn dial_and_handshake(
    endpoint: &Endpoint,
    ticket: &PipeTicket,
) -> Result<(Connection, SendStream, RecvStream, Option<u64>)> {
    let (conn, send, mut recv) = dial_and_authenticate(endpoint, ticket).await?;
    let mut header = [0u8; 8];
    recv.read_exact(&mut header)
        .await
        .context("reading the length header failed")?;
    let len = u64::from_le_bytes(header);
    Ok((conn, send, recv, (len != u64::MAX).then_some(len)))
}

/// True if the producer closed this connection because a newer consumer preempted
/// it (close code 2 — see `serve_follow`), as opposed to a network drop.
fn replaced_by_newer(conn: &Connection) -> bool {
    matches!(
        conn.close_reason(),
        Some(ConnectionError::ApplicationClosed(close)) if u64::from(close.error_code) == 2
    )
}

/// Single-shot consumer: dial, authenticate, and stream the whole source to
/// `writer`, reporting received bytes back so the producer's bar tracks delivery.
pub(crate) async fn transfer<W: AsyncWrite + Unpin>(
    endpoint: &Endpoint,
    ticket: &PipeTicket,
    writer: &mut W,
    throttle: Option<u64>,
) -> Result<()> {
    let (conn, mut send, mut recv, total) = dial_and_handshake(endpoint, ticket).await?;

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

/// Live-follow consumer (selected by the ticket's follow flag): dial once, then
/// stream the producer's live bytes to `writer`, flushing each chunk so a
/// `tail -f` appears as it arrives. Report-free — the producer does not read a
/// reverse channel in live mode.
///
/// # Errors
/// An unreachable producer (after the dial deadline), a pre-data drop, or a
/// dropped/preempted connection — the caller exits non-zero. A reconnect is just
/// re-running `ahsw pipe connect` with the same ticket; a preemption (a newer
/// `connect` took over) is reported with its own message.
pub(crate) async fn transfer_follow<W: AsyncWrite + Unpin>(
    endpoint: &Endpoint,
    ticket: &PipeTicket,
    writer: &mut W,
    throttle: Option<u64>,
) -> Result<()> {
    // Report-free in live mode; keep `_send` alive so the producer's paired recv
    // stays open, but we never write to it after the secret.
    let (conn, _send, mut recv, _total) = dial_and_handshake(endpoint, ticket).await?;

    // Show the OSC indicator only while bytes are actually arriving; tick it so a
    // long steady stream doesn't let the terminal fade it.
    let mut progress = Progress::hidden();
    let mut buf = vec![0u8; throttle_chunk(throttle)];
    let mut shown = false;
    let mut keepalive = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            result = AsyncReadExt::read(&mut recv, &mut buf) => match result {
                Ok(0) => break, // producer FIN — clean end of stream
                Ok(read) => {
                    if !shown {
                        shown = true;
                        progress.show();
                    }
                    writer
                        .write_all(&buf[..read])
                        .await
                        .context("writing to the sink failed")?;
                    writer.flush().await.context("flushing the sink failed")?;
                    pace(throttle, read).await;
                }
                Err(error) => {
                    progress.fail();
                    // An intended preemption (a newer `connect` took over, close
                    // code 2) is distinct from a network drop — report it as such,
                    // but still exit non-zero (the takeover ended this consumer).
                    if replaced_by_newer(&conn) {
                        return Err(anyhow::anyhow!(
                            "this pipe was taken over by a newer `ahsw pipe connect`"
                        ));
                    }
                    return Err(anyhow::Error::new(error).context("the pipe connection dropped"));
                }
            },
            _ = keepalive.tick(), if shown => progress.tick(),
        }
    }
    progress.finish();
    Ok(())
}
