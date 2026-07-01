//! The shell producer: spawn `$SHELL` in a PTY, mirror it to the local terminal,
//! and broadcast its output to every attached viewer. The sharer uses the shell
//! transparently (stdin is put in raw mode and copied into the PTY; the PTY's
//! output is copied to the local terminal *and* framed out to viewers). A bounded
//! replay buffer lets a viewer that joins mid-session start from the recent
//! output rather than a blank screen.

use std::io::{IsTerminal, Read, Write};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use iroh::Endpoint;
use iroh::endpoint::{Connection, Incoming, RecvStream, SendStream};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use rand::RngCore;
use tokio::io::AsyncWriteExt;

use crate::lookup::build_endpoint;
use crate::protocol::swarm::LookupOpts;

use super::term;
use super::ticket::ShTicket;
use super::{SECRET_LEN, SH_ALPN, wait_online};

/// Recent PTY output kept to prime late-joining viewers, so a viewer that
/// attaches mid-session starts from the current screen rather than a blank one.
/// Raw bytes (not a screen model) — bounded so it never grows without limit.
const REPLAY_CAP: usize = 256 * 1024;

/// Broadcast the local shell to viewers; prints the `ahsw sh connect <ticket>`
/// command on stdout, then runs the shell until it exits.
///
/// # Errors
/// Endpoint bind / discovery-config failures, PTY setup failure, or a fatal
/// stream I/O error while serving.
pub(crate) async fn listen(
    swarm: Option<&str>,
    json: bool,
    command: Option<&str>,
    cols: Option<u16>,
    rows: Option<u16>,
) -> Result<()> {
    let lookups = super::swarm_lookups(swarm)?;
    let (endpoint, ticket, secret) = bind(lookups).await?;
    super::announce(
        json,
        "this shell — viewers watch read-only",
        &format!("ahsw sh connect {}", ticket.encode()),
    );
    let result = serve(&endpoint, &secret, command, cols, rows, !json).await;
    // Restore the tty before exiting on either path (raw mode was entered while
    // sharing) — `process::exit` skips `Drop`, so this is explicit.
    term::restore();
    match result {
        // The shell exited: the session is over. Exit now rather than await the
        // multi-second `endpoint.close()` teardown (relay/DHT/mDNS).
        Ok(()) => std::process::exit(0),
        Err(error) => {
            endpoint.close().await;
            Err(error)
        }
    }
}

/// Bind the producer endpoint and mint its ticket + secret — no I/O, no print.
pub(crate) async fn bind(lookups: LookupOpts) -> Result<(Endpoint, ShTicket, [u8; SECRET_LEN])> {
    let endpoint = build_endpoint(&lookups, None, None, vec![SH_ALPN.to_vec()]).await?;
    if !lookups.is_loopback() {
        wait_online(&endpoint).await;
    }
    let mut secret = [0u8; SECRET_LEN];
    rand::rng().fill_bytes(&mut secret);
    let ticket = ShTicket {
        addr: endpoint.addr(),
        secret,
        lookups,
    };
    Ok((endpoint, ticket, secret))
}

/// One attached viewer. `_recv` is held (never read) to keep the reverse stream
/// open — dropping it would send the viewer a `STOP_SENDING`.
struct Viewer {
    conn: Connection,
    send: SendStream,
    _recv: RecvStream,
}

/// Spawn the shell in a PTY and run the fan-out loop until the shell exits (PTY
/// EOF) or the endpoint closes. `narrate` is currently unused by the producer's
/// hot path but kept for signature parity with the other producers.
async fn serve(
    endpoint: &Endpoint,
    secret: &[u8; SECRET_LEN],
    command: Option<&str>,
    cols_override: Option<u16>,
    rows_override: Option<u16>,
    _narrate: bool,
) -> Result<()> {
    // Size: the explicit test knobs win; otherwise the controlling tty's size.
    let (cols, rows) = if let (Some(cols), Some(rows)) = (cols_override, rows_override) {
        (cols, rows)
    } else {
        term::size()
    };
    // Interactive only with a real tty and no size override (the test path drives
    // a non-tty and must not touch the terminal).
    let interactive = cols_override.is_none() && std::io::stdin().is_terminal();

    let (mut out_rx, master) = spawn_pty_session(command, cols, rows, interactive)?;

    let (auth_tx, mut auth_rx) =
        tokio::sync::mpsc::channel::<(Connection, SendStream, RecvStream)>(1);
    let mut viewers: Vec<Viewer> = Vec::new();
    let mut replay: Vec<u8> = Vec::new();
    let mut stdout = tokio::io::stdout();
    let (mut cur_cols, mut cur_rows) = (cols, rows);
    let mut winch = if interactive {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change()).ok()
    } else {
        None
    };

    loop {
        tokio::select! {
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break };
                let secret = *secret;
                let auth_tx = auth_tx.clone();
                tokio::spawn(async move {
                    if let Ok(triple) = authenticate(incoming, &secret).await {
                        let _ = auth_tx.send(triple).await;
                    }
                });
            }
            Some((conn, mut send, recv)) = auth_rx.recv() => {
                // Prime the newcomer: current size, then the recent output (or a
                // clear if there's nothing buffered yet) so it starts on a coherent
                // screen — it joins live and can't reconstruct true scrollback.
                let primed = send.write_all(&super::encode_resize(cur_cols, cur_rows)).await.is_ok()
                    && if replay.is_empty() {
                        send.write_all(&super::encode_data(b"\x1b[2J\x1b[H")).await.is_ok()
                    } else {
                        send.write_all(&super::encode_data(&replay)).await.is_ok()
                    };
                if primed {
                    viewers.push(Viewer { conn, send, _recv: recv });
                }
            }
            Some(()) = maybe_winch(&mut winch) => {
                let (new_cols, new_rows) = term::size();
                let _ = master.resize(PtySize {
                    rows: new_rows,
                    cols: new_cols,
                    pixel_width: 0,
                    pixel_height: 0,
                });
                cur_cols = new_cols;
                cur_rows = new_rows;
                broadcast(&mut viewers, &super::encode_resize(new_cols, new_rows)).await;
            }
            chunk = out_rx.recv() => match chunk {
                None => {
                    // The shell exited: FIN each viewer so its `connect` ends
                    // cleanly, briefly awaiting delivery, then stop.
                    for viewer in std::mem::take(&mut viewers) {
                        let Viewer { conn, mut send, .. } = viewer;
                        let _ = send.finish();
                        let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
                        conn.close(0u32.into(), b"shell exited");
                    }
                    break;
                }
                Some(bytes) => {
                    push_replay(&mut replay, &bytes);
                    if interactive {
                        stdout
                            .write_all(&bytes)
                            .await
                            .context("writing to the local terminal failed")?;
                        let _ = stdout.flush().await;
                    }
                    broadcast(&mut viewers, &super::encode_data(&bytes)).await;
                }
            }
        }
    }
    Ok(())
}

/// The output stream of a spawned PTY session plus its master handle (kept for
/// resize). The blocking PTY reader is bridged to async over the mpsc.
type PtySession = (tokio::sync::mpsc::Receiver<Vec<u8>>, Box<dyn MasterPty + Send>);

/// Open a PTY, spawn the shell, and wire up the I/O threads: stdin → PTY (when
/// interactive), PTY output → an mpsc of chunks, and a child reaper. Returns the
/// output receiver and the master handle (kept by the caller for resize).
fn spawn_pty_session(
    command: Option<&str>,
    cols: u16,
    rows: u16,
    interactive: bool,
) -> Result<PtySession> {
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("opening a pty failed")?;
    let mut child = pair
        .slave
        .spawn_command(build_command(command))
        .context("spawning the shell failed")?;
    // Close the slave in the parent so the master read returns EOF once the child
    // exits — that EOF is our end-of-session signal.
    drop(pair.slave);
    let mut reader = pair
        .master
        .try_clone_reader()
        .context("cloning the pty reader failed")?;
    let master = pair.master;

    if interactive {
        term::enter_raw();
        let mut writer = master.take_writer().context("taking the pty writer failed")?;
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut buf = [0u8; 4096];
            while let Ok(read) = stdin.read(&mut buf) {
                if read == 0 || writer.write_all(&buf[..read]).is_err() {
                    break;
                }
                let _ = writer.flush();
            }
        });
    }

    // Bridge the blocking PTY reader into async: a thread reads the master and
    // hands each chunk to the hub over an mpsc.
    let (out_tx, out_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if out_tx.blocking_send(buf[..read].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    // Reap the child so it never lingers as a zombie (PTY EOF is the real signal).
    std::thread::spawn(move || {
        let _ = child.wait();
    });

    Ok((out_rx, master))
}

/// Await the next `SIGWINCH` if a handler is installed, else never resolve — so
/// the `select!` arm is simply inert when the producer isn't interactive.
async fn maybe_winch(sig: &mut Option<tokio::signal::unix::Signal>) -> Option<()> {
    match sig {
        Some(sig) => sig.recv().await,
        None => std::future::pending().await,
    }
}

/// Append `bytes` to the replay buffer, trimming the front so it never exceeds
/// [`REPLAY_CAP`]. A hard byte cap, not a screen model — enough to redraw the
/// current view for a late joiner in the common case.
fn push_replay(replay: &mut Vec<u8>, bytes: &[u8]) {
    replay.extend_from_slice(bytes);
    if replay.len() > REPLAY_CAP {
        let overflow = replay.len() - REPLAY_CAP;
        replay.drain(..overflow);
    }
}

/// Write `frame` to every viewer concurrently, dropping any whose write failed
/// (it went away). Faithful to the single-consumer backpressure this generalizes:
/// a slow viewer paces the broadcast (head-of-line).
async fn broadcast(viewers: &mut Vec<Viewer>, frame: &[u8]) {
    if viewers.is_empty() {
        return;
    }
    let results = futures_util::future::join_all(
        viewers.iter_mut().map(|viewer| viewer.send.write_all(frame)),
    )
    .await;
    let mut keep = results.into_iter().map(|write| write.is_ok());
    viewers.retain(|_| keep.next().unwrap_or(true));
}

/// Accept one incoming connection, take its bi-stream, and verify the bearer
/// secret (the viewer opens the bi-stream and writes the secret first).
///
/// # Errors
/// A failed handshake or a bad secret (closed with code 1).
async fn authenticate(
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

/// Build the command run inside the PTY: `--command` (a test/ops knob) runs via
/// `sh -c`; otherwise the sharer's `$SHELL` (falling back to `/bin/sh`). Inherits
/// the current working directory and ensures `TERM` is set.
fn build_command(command: Option<&str>) -> CommandBuilder {
    let mut cmd = if let Some(line) = command {
        let mut builder = CommandBuilder::new("/bin/sh");
        builder.arg("-c");
        builder.arg(line);
        builder
    } else {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
        CommandBuilder::new(shell)
    };
    if let Ok(cwd) = std::env::current_dir() {
        cmd.cwd(cwd);
    }
    if std::env::var_os("TERM").is_none() {
        cmd.env("TERM", "xterm-256color");
    }
    cmd
}
