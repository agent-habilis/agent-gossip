//! The pipe producer: read a source, print the consumer's `ahsw pipe connect`
//! command on stdout, then stream the source, once, to the first peer that
//! presents the ticket's secret.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use iroh::Endpoint;
use rand::RngCore;
use tokio::io::AsyncRead;

use crate::lookup::build_endpoint;
use crate::protocol::swarm::LookupOpts;

use super::progress::{Progress, copy_throttled};
use super::ticket::PipeTicket;
use super::{PIPE_ALPN, SECRET_LEN, wait_online};

/// Serve stdin to the first peer presenting the ticket's secret. The consumer's
/// **`ahsw pipe connect <ticket>` command is printed to stdout** (the producer's
/// stdout carries no data — that flows over the network); stderr is reserved for
/// errors. `swarm` selects the discovery config (`None` ⇒ a public default).
///
/// # Errors
/// Endpoint bind / discovery-config parse failures, or a stream I/O error.
pub(crate) async fn listen(swarm: Option<&str>, throttle: Option<u64>, json: bool) -> Result<()> {
    let lookups = super::swarm_lookups(swarm)?;
    let (endpoint, ticket, secret) = bind(lookups).await?;
    super::announce(
        json,
        "waiting for a peer to connect…",
        &format!("ahsw pipe connect {}", ticket.encode()),
    );
    match serve(
        &endpoint,
        &secret,
        &mut tokio::io::stdin(),
        stdin_len(),
        throttle,
        !json,
    )
    .await
    {
        // The peer confirmed receipt (`conn.closed`); exit now rather than await
        // the multi-second `endpoint.close()` teardown (relay/DHT/mDNS).
        // `process::exit` also skips the endpoint's abort-logging `Drop`.
        Ok(()) => std::process::exit(0),
        Err(error) => {
            endpoint.close().await;
            Err(error)
        }
    }
}

/// Bind the producer endpoint and mint its ticket + secret — no I/O, no print.
pub(crate) async fn bind(lookups: LookupOpts) -> Result<(Endpoint, PipeTicket, [u8; SECRET_LEN])> {
    let endpoint = build_endpoint(&lookups, None, None, vec![PIPE_ALPN.to_vec()]).await?;
    // Loopback needs no online wait (the bound addr is immediately usable).
    if !lookups.is_loopback() {
        wait_online(&endpoint).await;
    }
    let mut secret = [0u8; SECRET_LEN];
    rand::rng().fill_bytes(&mut secret);
    let ticket = PipeTicket {
        addr: endpoint.addr(),
        secret,
        lookups,
    };
    Ok((endpoint, ticket, secret))
}

/// Stream `reader` to the first peer that opens a bi-stream and presents
/// `secret`. Returns once the whole source is delivered and the peer closes.
///
/// # Errors
/// The endpoint closing before a peer connects, or a stream I/O error.
pub(crate) async fn serve<R: AsyncRead + Unpin>(
    endpoint: &Endpoint,
    secret: &[u8; SECRET_LEN],
    reader: &mut R,
    total: Option<u64>,
    throttle: Option<u64>,
    narrate: bool,
) -> Result<()> {
    let (_conn, mut send, mut recv) = loop {
        let Some(incoming) = endpoint.accept().await else {
            bail!("endpoint closed before a peer connected");
        };
        let conn = match incoming.await {
            Ok(conn) => conn,
            Err(error) => {
                tracing::debug!(%error, "incoming connection failed");
                continue;
            }
        };
        // The consumer opens the bi-stream and writes the secret first.
        let (send, mut recv) = match conn.accept_bi().await {
            Ok(streams) => streams,
            Err(error) => {
                tracing::debug!(%error, "accept_bi failed");
                continue;
            }
        };
        let mut got = [0u8; SECRET_LEN];
        if recv.read_exact(&mut got).await.is_err() || &got != secret {
            tracing::debug!("peer presented a bad secret; rejecting");
            conn.close(1u32.into(), b"bad secret");
            continue;
        }
        break (conn, send, recv);
    };
    super::stage(narrate, "connected");

    // Length header (u64::MAX = unknown) before the data — lets the consumer
    // size its determinate bar.
    send.write_all(&total.unwrap_or(u64::MAX).to_le_bytes())
        .await
        .context("sending the length header failed")?;
    super::stage(narrate, "transferring…");

    // Send the data while concurrently reading the consumer's received-byte
    // reports, so our bar reflects real delivery (not bytes merely queued to
    // QUIC). The report stream's FIN is the delivery confirmation. Disjoint
    // borrows: send/reader vs recv/progress.
    let mut progress = Progress::new(total);
    let send_task = async {
        copy_throttled(reader, &mut send, throttle)
            .await
            .context("streaming the source to the peer failed")?;
        send.finish().context("finishing the stream failed")?;
        Ok::<(), anyhow::Error>(())
    };
    let report_task = async {
        // The consumer waits on `send.stopped()` before exiting, so its reports
        // and FIN are reliably delivered — this `read_exact` loop always reaches
        // the FIN rather than hanging on a dropped frame.
        let mut buf = [0u8; 8];
        if total.is_some() {
            // Determinate: each report advances the percent, which keeps the bar
            // alive on its own.
            while recv.read_exact(&mut buf).await.is_ok() {
                progress.update(u64::from_le_bytes(buf));
            }
        } else {
            // Indeterminate: no per-% updates arrive, so re-emit the "loading"
            // state periodically (else the terminal fades it mid-transfer). Only
            // the final report + FIN come over the stream; ignore the count.
            let mut keepalive = tokio::time::interval(Duration::from_millis(250));
            loop {
                tokio::select! {
                    result = recv.read_exact(&mut buf) => {
                        if result.is_err() { break; }
                    }
                    _ = keepalive.tick() => progress.tick(),
                }
            }
        }
        progress.finish();
    };
    let (outcome, ()) = tokio::join!(send_task, report_task);
    if outcome.is_ok() {
        super::stage(narrate, "finished");
    }
    outcome
}

/// The byte length of stdin when it is a regular file (`ahsw pipe listen < file`),
/// else `None` — a pipe / FIFO (`cat file |`, `tail -f |`) hides any length, so
/// the receiver shows an indeterminate indicator instead.
#[expect(
    unsafe_code,
    reason = "fstat(2)/lseek(2) on stdin (fd 0) to size a regular-file source"
)]
fn stdin_len() -> Option<u64> {
    // SAFETY: fstat/lseek read only POD scalar fields of a zeroed `libc::stat`
    // for the raw fd 0.
    unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        if libc::fstat(0, &raw mut st) != 0 {
            return None;
        }
        if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return None;
        }
        let size = u64::try_from(st.st_size).ok()?;
        let consumed = u64::try_from(libc::lseek(0, 0, libc::SEEK_CUR)).unwrap_or(0);
        Some(size.saturating_sub(consumed))
    }
}
