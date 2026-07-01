//! The pipe producer: read a source, print the consumer's `ahsw pipe connect`
//! command on stdout, then stream the source, once, to the first peer that
//! presents the ticket's secret.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use iroh::Endpoint;
use iroh::endpoint::{Connection, Incoming, RecvStream, SendStream};
use rand::RngCore;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::lookup::build_endpoint;
use crate::protocol::swarm::LookupOpts;

use super::progress::{Progress, copy_throttled, pace, throttle_chunk};
use super::ticket::PipeTicket;
use super::{PIPE_ALPN, SECRET_LEN, wait_online};

/// Serve stdin to the first peer presenting the ticket's secret. The consumer's
/// **`ahsw pipe connect <ticket>` command is printed to stdout** (the producer's
/// stdout carries no data — that flows over the network); stderr is reserved for
/// errors. `swarm` selects the discovery config (`None` ⇒ a public default).
///
/// # Errors
/// Endpoint bind / discovery-config parse failures, or a stream I/O error.
pub(crate) async fn listen(
    swarm: Option<&str>,
    throttle: Option<u64>,
    json: bool,
    follow: bool,
) -> Result<()> {
    let lookups = super::swarm_lookups(swarm)?;
    let (endpoint, mut ticket, secret) = bind(lookups).await?;
    ticket.follow = follow;
    super::announce(
        json,
        "Waiting",
        "for a peer to connect",
        &format!("ahsw pipe connect {}", ticket.encode()),
    );
    let result = if follow {
        serve_follow(&endpoint, &secret, &mut tokio::io::stdin(), throttle, !json).await
    } else {
        serve(
            &endpoint,
            &secret,
            &mut tokio::io::stdin(),
            stdin_len(),
            throttle,
            !json,
        )
        .await
    };
    match result {
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
        follow: false,
        target_port: None,
        bench: false,
    };
    Ok((endpoint, ticket, secret))
}

/// Accept one incoming connection, take its bi-stream, and verify the bearer
/// secret. The consumer opens the bi-stream and writes the secret first.
/// `pub(super)`: shared with `bench.rs`, which runs its own protocol after
/// authenticating rather than `serve`'s length-header + byte-stream one.
///
/// # Errors
/// A failed handshake or a bad secret (the caller drops the connection and
/// waits for another); a bad secret is closed with code 1.
pub(super) async fn authenticate(
    incoming: Incoming,
    secret: &[u8; SECRET_LEN],
) -> Result<(Connection, SendStream, RecvStream)> {
    let conn = incoming.await.context("incoming connection failed")?;
    let (send, mut recv) = conn.accept_bi().await.context("accept_bi failed")?;
    let mut got = [0u8; SECRET_LEN];
    if recv.read_exact(&mut got).await.is_err() || &got != secret {
        conn.close(1u32.into(), b"bad secret");
        bail!("peer presented a bad secret");
    }
    Ok((conn, send, recv))
}

/// Live-follow producer (`pipe listen --follow`): forward `reader` to the one
/// attached consumer, reading the source ONLY while a consumer is attached — so a
/// fast non-blocking source (e.g. `/dev/random`) never busy-spins while idle, and
/// a blocking source like `tail -f` just backpressures. One consumer at a time; a
/// newer `connect` preempts the current one, so a reconnect is instant even when
/// the old consumer died abruptly. On a drop the slot frees and the producer
/// waits for a fresh `connect` with the same ticket — it never quits on a drop,
/// only on source EOF (after cleanly FIN-ing any attached consumer). A
/// (re)connecting consumer joins at roughly the live tip — it may first drain a
/// small buffered backlog (the OS pipe buffer), then follows live. The OSC
/// indicator (shown only while transferring) and the lifecycle stage lines —
/// `disconnected` / `connected` / `transferring` / `finished` — mark the state.
///
/// # Errors
/// A stdin read error, or the endpoint closing before the source ends.
pub(crate) async fn serve_follow<R: AsyncRead + Unpin>(
    endpoint: &Endpoint,
    secret: &[u8; SECRET_LEN],
    reader: &mut R,
    throttle: Option<u64>,
    narrate: bool,
) -> Result<()> {
    // Authenticated peers arrive here off the accept hot path (like tcp.rs).
    let (auth_tx, mut auth_rx) =
        tokio::sync::mpsc::channel::<(Connection, SendStream, RecvStream)>(1);
    // The single live-consumer slot (holds `recv` only to suppress STOP_SENDING).
    let mut current: Option<(Connection, SendStream, RecvStream)> = None;
    let mut transferring = false;
    // Indicator hidden until bytes actually flow, cleared the moment they stop.
    let mut progress = Progress::hidden();
    let mut buf = vec![0u8; throttle_chunk(throttle)];
    // Re-emit the indeterminate OSC indicator periodically while transferring, so
    // it doesn't fade during a steady stream (mirrors the non-follow path).
    let mut keepalive = tokio::time::interval(Duration::from_millis(250));

    super::stage(narrate, "disconnected");
    loop {
        // A cheap per-iteration Arc clone, so the `closed()` future borrows a
        // local and never the mutable `current` slot.
        let watch = current.as_ref().map(|(conn, _, _)| conn.clone());
        let closed = async {
            match &watch {
                Some(conn) => {
                    conn.closed().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            // A peer is dialing — authenticate off the hot path.
            incoming = endpoint.accept() => {
                // None = the endpoint was closed out from under us; unlike source
                // EOF that is a failure, so surface it (non-follow serve bails too).
                let Some(incoming) = incoming else {
                    bail!("endpoint closed before the source ended");
                };
                let secret = *secret;
                let auth_tx = auth_tx.clone();
                tokio::spawn(async move {
                    if let Ok(triple) = authenticate(incoming, &secret).await {
                        let _ = auth_tx.send(triple).await;
                    }
                });
            }
            // An authenticated consumer is ready — it takes the single slot,
            // preempting whatever was there. A reconnect is then instant even
            // when the previous consumer died abruptly and its connection is
            // still a zombie (iroh would only idle-time it out much later).
            Some((conn, mut send, recv)) = auth_rx.recv() => {
                // Header first: a half-dead newcomer that can't take the header
                // must not evict a live consumer. Live = indeterminate length.
                if send.write_all(&u64::MAX.to_le_bytes()).await.is_ok() {
                    if let Some((old, _, _)) = current.take() {
                        old.close(2u32.into(), b"replaced by a newer consumer");
                        progress.finish();
                        super::stage(narrate, "disconnected");
                    }
                    super::stage(narrate, "connected");
                    transferring = false;
                    current = Some((conn, send, recv));
                }
            }
            // The attached consumer dropped while idle — free the slot.
            () = closed => {
                if current.take().is_some() {
                    progress.finish();
                    super::stage(narrate, "disconnected");
                    transferring = false;
                }
            }
            // Keep the OSC indicator alive during a steady transfer (it fades on
            // its own after an idle stretch). Only fires while transferring.
            _ = keepalive.tick(), if transferring => progress.tick(),
            // Next source chunk, or EOF — read ONLY while a consumer is attached,
            // so a fast non-blocking source never busy-spins while idle and we
            // never drain the source to nowhere.
            read = reader.read(&mut buf), if current.is_some() => match read {
                Ok(0) => {
                    // Source closed — FIN the consumer and wait (briefly) for it
                    // to ACK delivery, but never hang on one that already left.
                    if let Some((conn, mut send, _)) = current.take() {
                        let _ = send.finish();
                        let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
                        conn.close(0u32.into(), b"done");
                    }
                    progress.finish();
                    super::stage(narrate, "finished");
                    break;
                }
                Ok(read) => {
                    if !transferring {
                        transferring = true;
                        super::stage(narrate, "transferring…");
                        progress.show();
                    }
                    let mut dropped = false;
                    if let Some((_, send, _)) = current.as_mut() {
                        match send.write_all(&buf[..read]).await {
                            Ok(()) => pace(throttle, read).await,
                            Err(_) => dropped = true,
                        }
                    }
                    if dropped {
                        current = None;
                        progress.finish();
                        super::stage(narrate, "disconnected");
                        transferring = false;
                    }
                }
                Err(error) => {
                    return Err(anyhow::Error::new(error).context("reading stdin failed"));
                }
            }
        }
    }
    Ok(())
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
        match authenticate(incoming, secret).await {
            Ok(triple) => break triple,
            Err(error) => tracing::debug!(%error, "consumer handshake failed; awaiting another"),
        }
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
