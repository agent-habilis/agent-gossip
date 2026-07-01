//! The pipe producer: read a source, print the consumer's `ahsw pipe connect`
//! command on stdout, then serve it. A seekable file fans out — every consumer
//! gets its own full copy, re-opened per connection; `--follow` broadcasts the
//! live source to all attached consumers; a non-seekable stream (a pipe, which
//! can't be replayed) is served once to the first consumer, then exits.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use iroh::Endpoint;
use iroh::endpoint::{Connection, Incoming, RecvStream, SendStream};
use rand::RngCore;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::directory::ticket::TicketAd;
use crate::lookup::build_endpoint;
use crate::protocol::swarm::{
    DirectorySelection, LookupOpts, LookupSet, resolve_transfer_lookups, validate_advertise,
};

use super::progress::{Progress, copy_throttled, pace, throttle_chunk};
use super::ticket::PipeTicket;
use super::{PIPE_ALPN, SECRET_LEN, wait_online};

/// Serve stdin to the first peer presenting the ticket's secret. The consumer's
/// **`ahsw pipe connect <ticket>` command is printed to stdout** (the producer's
/// stdout carries no data — that flows over the network); stderr is reserved for
/// errors. The discovery config comes from `swarm` (a `🐝…` id's embedded
/// lookups) or `flags` (create-style `--mdns`/`--dht`/`--relay`); neither ⇒ a
/// public default. `advertise` additionally re-broadcasts the ticket into a
/// directory so a peer can find it with `ahsw pipe discover`.
///
/// # Errors
/// Endpoint bind / discovery-config resolution failures, `--advertise` on an
/// unreachable config or a source that can't be re-served, or a stream I/O
/// error.
pub(crate) async fn listen(
    swarm: Option<&str>,
    flags: LookupSet,
    advertise: DirectorySelection,
    throttle: Option<u64>,
    json: bool,
    follow: bool,
) -> Result<()> {
    let lookups = resolve_transfer_lookups(swarm, flags)?;
    validate_advertise(&advertise, &lookups)?;
    // An advertised ticket must stay redeemable: a non-seekable stdin stream
    // is served once and gone, so only a re-openable file or --follow qualifies.
    if advertise.is_set() && !follow && source_path().is_none() {
        bail!(
            "--advertise needs a re-servable source: \
             redirect a seekable file (`< file`) or pass --follow"
        );
    }
    let (endpoint, mut ticket, secret) = bind(lookups.clone()).await?;
    ticket.follow = follow;
    let _advertiser = match advertise.directory() {
        Some(directory) => {
            let label = source_path()
                .as_deref()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .map(str::to_owned);
            let ad = TicketAd {
                ticket: ticket.encode(),
                label,
            };
            if !json {
                crate::util::output::status("Advertising", &format!("in #{directory} directory"));
            }
            Some(crate::embed::spawn_ticket_advertiser(directory, lookups, &ad)?)
        }
        None => None,
    };
    super::announce(
        json,
        "Waiting",
        "for a peer to connect",
        &format!("ahsw pipe connect {}", ticket.encode()),
    );
    if follow {
        // Live tail: broadcast to every attached consumer until the source ends.
        return match serve_follow(&endpoint, &secret, &mut tokio::io::stdin(), throttle, !json).await
        {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                endpoint.close().await;
                Err(error)
            }
        };
    }
    // A seekable file can be re-opened per consumer, so fan out: stay up and
    // serve the whole file to each peer independently (like `port`). Runs until
    // interrupted, so — unlike the single-shot path — it does not `process::exit`.
    if let Some(path) = source_path() {
        let result = serve_fanout(&endpoint, &secret, &path, throttle).await;
        endpoint.close().await;
        return result;
    }
    // A non-seekable stream (a pipe, e.g. `tar c … |`) can't be replayed, so
    // there is no fan-out: serve the first consumer once, then exit.
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
        follow: false,
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

/// One attached live-follow consumer. `_recv` is held (never read) only to keep
/// the reverse stream open — dropping it would send the consumer a `STOP_SENDING`.
struct FollowConsumer {
    conn: Connection,
    send: SendStream,
    _recv: RecvStream,
}

/// Live-follow producer (`pipe listen --follow`): broadcast `reader` to every
/// attached consumer, reading the source ONLY while at least one is attached — so
/// a fast non-blocking source (e.g. `/dev/random`) never busy-spins while idle,
/// and a blocking source like `tail -f` just backpressures. Any number of
/// consumers attach at once (a new `connect` joins the fan-out; it does not
/// preempt); each joins at roughly the live tip. A dropped consumer is dropped
/// from the set (on its next failed write, or reaped when idle), and the producer
/// quits only on source EOF — never on a consumer leaving — after cleanly FIN-ing
/// whoever remains. The OSC indicator (shown only while transferring) and the
/// lifecycle stage lines — `disconnected` / `connected` / `transferring` /
/// `finished` — mark the state.
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
    // Authenticated peers arrive here off the accept hot path (like port).
    let (auth_tx, mut auth_rx) =
        tokio::sync::mpsc::channel::<(Connection, SendStream, RecvStream)>(1);
    let mut consumers: Vec<FollowConsumer> = Vec::new();
    let mut transferring = false;
    // Indicator hidden until bytes actually flow, cleared the moment they stop.
    let mut progress = Progress::hidden();
    let mut buf = vec![0u8; throttle_chunk(throttle)];
    // Re-emit the indeterminate OSC indicator periodically while transferring, so
    // it doesn't fade during a steady stream; the same tick reaps idle drops.
    let mut keepalive = tokio::time::interval(Duration::from_millis(250));

    super::stage(narrate, "disconnected");
    loop {
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
            // An authenticated consumer is ready — it joins the fan-out set.
            Some((conn, mut send, recv)) = auth_rx.recv() => {
                // Header first: a half-dead newcomer that can't take the header is
                // not added. Live = indeterminate length.
                if send.write_all(&u64::MAX.to_le_bytes()).await.is_ok() {
                    let was_empty = consumers.is_empty();
                    consumers.push(FollowConsumer { conn, send, _recv: recv });
                    if was_empty {
                        super::stage(narrate, "connected");
                    }
                }
            }
            // Keep the OSC indicator alive during a steady transfer, and reap any
            // consumer that left while idle (no write was in flight to notice).
            _ = keepalive.tick() => {
                consumers.retain(|consumer| consumer.conn.close_reason().is_none());
                if consumers.is_empty() && transferring {
                    progress.finish();
                    super::stage(narrate, "disconnected");
                    transferring = false;
                } else if transferring {
                    progress.tick();
                }
            }
            // Next source chunk, or EOF — read ONLY while a consumer is attached,
            // so a fast non-blocking source never busy-spins while idle and we
            // never drain the source to nowhere.
            read = reader.read(&mut buf), if !consumers.is_empty() => match read {
                Ok(0) => {
                    // Source closed — FIN each consumer and wait (briefly) for it
                    // to ACK delivery, but never hang on one that already left.
                    for consumer in std::mem::take(&mut consumers) {
                        let FollowConsumer { conn, mut send, .. } = consumer;
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
                    // Broadcast the chunk to every consumer concurrently; a write
                    // paces the source to the slowest (head-of-line), faithful to
                    // the single-consumer backpressure this generalizes.
                    let chunk = &buf[..read];
                    let results =
                        futures_util::future::join_all(
                            consumers.iter_mut().map(|consumer| consumer.send.write_all(chunk)),
                        )
                        .await;
                    // Drop consumers whose write failed (they went away). The
                    // results are in consumer order, so zip them back by iteration.
                    let mut keep = results.into_iter().map(|write| write.is_ok());
                    consumers.retain(|_| keep.next().unwrap_or(true));
                    pace(throttle, read).await;
                    if consumers.is_empty() {
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
    let (_conn, send, recv) = loop {
        let Some(incoming) = endpoint.accept().await else {
            bail!("endpoint closed before a peer connected");
        };
        match authenticate(incoming, secret).await {
            Ok(triple) => break triple,
            Err(error) => tracing::debug!(%error, "consumer handshake failed; awaiting another"),
        }
    };
    super::stage(narrate, "connected");
    super::stage(narrate, "transferring…");
    let outcome = serve_one_stream(send, recv, reader, total, throttle, narrate).await;
    if outcome.is_ok() {
        super::stage(narrate, "finished");
    }
    outcome
}

/// Fan-out producer for a seekable file: serve the whole file to every consumer
/// independently, re-opening `path` per connection so each gets its own byte-0
/// offset. Stays up serving many (like `port`) until the endpoint closes; a bad
/// handshake or a dropped consumer only ends that one connection.
///
/// # Errors
/// Never returns `Err` in normal operation — the accept loop ends only when the
/// endpoint is closed, and per-consumer failures are logged, not propagated.
pub(super) async fn serve_fanout(
    endpoint: &Endpoint,
    secret: &[u8; SECRET_LEN],
    path: &Path,
    throttle: Option<u64>,
) -> Result<()> {
    while let Some(incoming) = endpoint.accept().await {
        let secret = *secret;
        let path = path.to_owned();
        tokio::spawn(async move {
            if let Err(error) = serve_file_to(incoming, &secret, &path, throttle).await {
                tracing::debug!(%error, "fan-out consumer ended");
            }
        });
    }
    Ok(())
}

/// One fan-out connection: authenticate, re-open the file, and stream its whole
/// contents. Progress is suppressed (many concurrent consumers can't share one
/// stdout bar); lifecycle is logged instead.
async fn serve_file_to(
    incoming: Incoming,
    secret: &[u8; SECRET_LEN],
    path: &Path,
    throttle: Option<u64>,
) -> Result<()> {
    let (_conn, send, recv) = authenticate(incoming, secret).await?;
    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("re-opening {} failed", path.display()))?;
    let total = file.metadata().await.ok().map(|meta| meta.len());
    tracing::info!("consumer connected");
    let result = serve_one_stream(send, recv, &mut file, total, throttle, false).await;
    tracing::info!("consumer disconnected");
    result
}

/// Send the length header, stream `reader` to `send`, and concurrently drain the
/// consumer's received-byte reports off `recv` — so the bar reflects real
/// delivery (not bytes merely queued to QUIC), and the consumer's report writes
/// never flow-control-stall on an unread stream. The report stream's FIN is the
/// delivery confirmation. `narrate` gates the visible progress bar (off for
/// fan-out, where many consumers would clash on one stdout).
async fn serve_one_stream<R: AsyncRead + Unpin>(
    mut send: SendStream,
    mut recv: RecvStream,
    reader: &mut R,
    total: Option<u64>,
    throttle: Option<u64>,
    narrate: bool,
) -> Result<()> {
    // Length header (u64::MAX = unknown) before the data — lets the consumer
    // size its determinate bar.
    send.write_all(&total.unwrap_or(u64::MAX).to_le_bytes())
        .await
        .context("sending the length header failed")?;

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
                if narrate {
                    progress.update(u64::from_le_bytes(buf));
                }
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
                    _ = keepalive.tick() => {
                        if narrate { progress.tick(); }
                    }
                }
            }
        }
        if narrate {
            progress.finish();
        }
    };
    let (outcome, ()) = tokio::join!(send_task, report_task);
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

/// The filesystem path behind stdin (fd 0) when it is a **re-openable regular
/// file** (`ahsw pipe listen < file`), else `None`. `Some` selects the fan-out
/// path — each consumer re-opens this to get its own full copy; `None` (a pipe /
/// FIFO like `cat file |`, which can't be replayed) selects the single-shot path.
#[cfg(target_os = "linux")]
fn source_path() -> Option<PathBuf> {
    // `/proc/self/fd/0` resolves to the underlying file; require it be regular so
    // a pipe/socket/tty (whose link is not a real re-openable path) falls back.
    let path = std::fs::read_link("/proc/self/fd/0").ok()?;
    std::fs::metadata(&path)
        .ok()
        .filter(std::fs::Metadata::is_file)
        .map(|_| path)
}

#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "fstat(2) to gate on a regular file + fcntl(F_GETPATH) to resolve fd 0's path"
)]
fn source_path() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;

    // SAFETY: fstat reads only POD scalar fields of a zeroed `libc::stat` for fd 0.
    unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        if libc::fstat(0, &raw mut st) != 0 || (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return None;
        }
    }
    let mut buf = [0u8; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH writes a NUL-terminated path (≤ PATH_MAX bytes) into the
    // buffer, whose capacity is exactly PATH_MAX.
    let ret = unsafe { libc::fcntl(0, libc::F_GETPATH, buf.as_mut_ptr().cast::<libc::c_char>()) };
    if ret != 0 {
        return None;
    }
    let len = buf.iter().position(|&byte| byte == 0)?;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..len])))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn source_path() -> Option<PathBuf> {
    None
}
