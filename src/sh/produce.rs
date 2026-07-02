use std::io::{IsTerminal, Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use iroh::Endpoint;
use iroh::endpoint::{Connection, Incoming, RecvStream, SendStream};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use rand::RngCore;
use tokio::io::AsyncWriteExt;

use crate::lookup::build_endpoint;
use crate::protocol::crypto::{Password, TicketAuth, ct_eq};
use crate::protocol::swarm::LookupOpts;

use super::state_file::ShStateFile;
use super::term;
use super::ticket::ShTicket;
use super::{SECRET_LEN, SH_ALPN, wait_online};

/// Recent PTY output kept to prime late-joining viewers, so a viewer that
/// attaches mid-session starts from the current screen rather than a blank one.
/// Raw bytes (not a screen model) — bounded so it never grows without limit.
const REPLAY_CAP: usize = 256 * 1024;

/// Evict a viewer whose stream can't accept a frame within this window, so a
/// viewer that stopped reading can't stall the fan-out (and with it the
/// sharer's shell) indefinitely.
const VIEWER_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Broadcast the local shell to viewers; prints the `ahsw sh connect <ticket>`
/// command on stdout, then runs the shell until it exits.
///
/// # Errors
/// Endpoint bind / discovery-config failures, PTY setup failure, or a fatal
/// stream I/O error while serving.
pub(crate) async fn listen(
    swarm: Option<&str>,
    json: bool,
    write: bool,
    command: Option<&str>,
    cols: Option<u16>,
    rows: Option<u16>,
    password: Option<Password>,
) -> Result<()> {
    let lookups = super::swarm_lookups(swarm)?;
    let (endpoint, ticket, write_ticket, secrets) =
        bind(lookups, write, password.as_ref()).await?;
    // The endpoint id (public) keys the state file the way `swarm_prefix` keys
    // a swarm's runtime dir; the same prefix rides into the shell as `AHSW_SH`.
    let sh_prefix: String = endpoint.id().to_string().chars().take(16).collect();
    let mut state_file =
        ShStateFile::new(super::state_file::path_for(&sh_prefix), &sh_prefix, swarm);
    let read_cmd = format!("ahsw sh connect {}", ticket.encode());
    if let Some(write_ticket) = write_ticket {
        // Two tickets: label each with its capability so they can't be mixed up.
        let write_cmd = format!("ahsw sh connect {}", write_ticket.encode());
        tracing::info!("sharing this shell with read and write tickets");
        if json {
            println!("{read_cmd}");
            println!("{write_cmd}");
        } else {
            crate::util::output::status_out(
                "Sharing",
                "this shell — the read ticket watches, the write ticket types",
            );
            crate::util::output::status_out(
                "Read",
                &format!("{read_cmd}   — watch only, safe to share"),
            );
            crate::util::output::status_out(
                "Write",
                &format!("{write_cmd}   — full shell control, share with care"),
            );
        }
    } else {
        super::announce(json, "this shell — viewers watch read-only", &read_cmd);
    }
    let result = serve(
        &endpoint,
        secrets,
        command,
        cols,
        rows,
        !json,
        &mut state_file,
    )
    .await;
    // Restore the tty before exiting on either path (raw mode was entered while
    // sharing) — `process::exit` skips `Drop`, so this is explicit; the state
    // file's removal is explicit for the same reason.
    term::restore();
    state_file.remove();
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

/// The producer's expected bearer tokens: matching `read` grants viewing;
/// matching `write` (minted only with `--write`) additionally grants typing.
/// Which token a viewer presents *is* the capability check — the ticket flag is
/// UX only. Without a password a token is the raw ticket secret; with one it is
/// the Argon2id stretch of the password salted by that secret, so a passworded
/// ticket admits no one who lacks the password.
#[derive(Clone, Copy)]
pub(crate) struct Secrets {
    read: [u8; SECRET_LEN],
    write: Option<[u8; SECRET_LEN]>,
}

/// Bind the producer endpoint and mint its ticket(s) + expected tokens — no
/// I/O, no print. The write ticket exists only when `write` is set. A single
/// `password` protects both tickets (each secret is stretched independently).
pub(crate) async fn bind(
    lookups: LookupOpts,
    write: bool,
    password: Option<&Password>,
) -> Result<(Endpoint, ShTicket, Option<ShTicket>, Secrets)> {
    let endpoint = build_endpoint(&lookups, None, None, vec![SH_ALPN.to_vec()]).await?;
    if !lookups.is_loopback() {
        wait_online(&endpoint).await;
    }
    let mut read_secret = [0u8; SECRET_LEN];
    rand::rng().fill_bytes(&mut read_secret);
    let write_secret = write.then(|| {
        let mut secret = [0u8; SECRET_LEN];
        rand::rng().fill_bytes(&mut secret);
        secret
    });
    let read_auth = TicketAuth::derive(&read_secret, password);
    let write_auth = write_secret.map(|secret| TicketAuth::derive(&secret, password));
    let protected = read_auth.password_protected;
    let ticket = ShTicket {
        addr: endpoint.addr(),
        secret: read_secret,
        lookups: lookups.clone(),
        write: false,
        password: protected,
    };
    let write_ticket = write_secret.map(|secret| ShTicket {
        addr: endpoint.addr(),
        secret,
        lookups,
        write: true,
        password: protected,
    });
    let secrets = Secrets {
        read: read_auth.token,
        write: write_auth.map(|auth| auth.token),
    };
    Ok((endpoint, ticket, write_ticket, secrets))
}

/// One attached viewer on the broadcast path. The reverse stream lives in the
/// viewer's own reader task (see `spawn_viewer_reader`), which closes the
/// connection on a protocol fault; the next broadcast write then evicts it here.
struct Viewer {
    conn: Connection,
    send: SendStream,
}

/// Spawn the shell in a PTY and run the fan-out loop until the shell exits (PTY
/// EOF) or the endpoint closes. `narrate` is currently unused by the producer's
/// hot path but kept for signature parity with the other producers.
pub(super) async fn serve(
    endpoint: &Endpoint,
    secrets: Secrets,
    command: Option<&str>,
    cols_override: Option<u16>,
    rows_override: Option<u16>,
    _narrate: bool,
    state_file: &mut ShStateFile,
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
    let write_enabled = secrets.write.is_some();

    // Written before the spawn so the file exists by the time the child's
    // first prompt renders.
    state_file.update(0);
    let sh_prefix = state_file.sh_prefix().to_owned();
    let (mut out_rx, input_tx, master) =
        spawn_pty_session(command, cols, rows, interactive, write_enabled, &sh_prefix)?;

    let (auth_tx, mut auth_rx) =
        tokio::sync::mpsc::channel::<(Connection, SendStream, RecvStream, bool)>(1);
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
                let auth_tx = auth_tx.clone();
                tokio::spawn(async move {
                    if let Ok(authed) = authenticate(incoming, secrets).await {
                        let _ = auth_tx.send(authed).await;
                    }
                });
            }
            Some((conn, mut send, recv, can_write)) = auth_rx.recv() => {
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
                    let viewer_input = if can_write { input_tx.clone() } else { None };
                    spawn_viewer_reader(conn.clone(), recv, viewer_input);
                    viewers.push(Viewer { conn, send });
                }
                state_file.update(viewers.len());
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
                state_file.update(viewers.len());
            }
            chunk = out_rx.recv() => match chunk {
                None => {
                    // The shell exited: FIN every viewer concurrently so its
                    // `connect` ends cleanly, briefly awaiting delivery, then stop
                    // — concurrent so stalled viewers cost 2s total, not 2s each.
                    let closers = std::mem::take(&mut viewers).into_iter().map(|viewer| async move {
                        let Viewer { conn, mut send } = viewer;
                        let _ = send.finish();
                        let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
                        conn.close(0u32.into(), b"shell exited");
                    });
                    futures_util::future::join_all(closers).await;
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
                    state_file.update(viewers.len());
                }
            }
        }
    }
    Ok(())
}

/// The output stream of a spawned PTY session, the channel carrying remote
/// write-viewer input to its writer (present only when write-enabled), and its
/// master handle (kept for resize). The blocking PTY reader/writer are bridged
/// to async over the mpscs.
type PtySession = (
    tokio::sync::mpsc::Receiver<Vec<u8>>,
    Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
    Box<dyn MasterPty + Send>,
);

/// Open a PTY, spawn the shell, and wire up the I/O threads: local stdin and
/// remote input → PTY writer (mutex-arbitrated so remote floods can't starve
/// the local keyboard), PTY output → an mpsc of chunks, and a child reaper.
/// Returns the output receiver, the remote-input sender, and the master handle
/// (kept by the caller for resize).
fn spawn_pty_session(
    command: Option<&str>,
    cols: u16,
    rows: u16,
    interactive: bool,
    write_enabled: bool,
    sh_prefix: &str,
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
        .spawn_command(build_command(command, sh_prefix))
        .context("spawning the shell failed")?;
    // Close the slave in the parent so the master read returns EOF once the child
    // exits — that EOF is our end-of-session signal.
    drop(pair.slave);
    let mut reader = pair
        .master
        .try_clone_reader()
        .context("cloning the pty reader failed")?;
    let master = pair.master;

    // `take_writer` is once-only, so a mutex arbitrates it between typists.
    // Local stdin writes under the lock directly while remote write viewers go
    // through a bounded channel drained by its own thread — a viewer flooding
    // input can therefore queue at most one in-flight chunk ahead of the
    // sharer's own keys, never the whole channel.
    let input_tx = if interactive || write_enabled {
        let writer = Arc::new(Mutex::new(
            master
                .take_writer()
                .context("taking the pty writer failed")?,
        ));

        let remote_tx = if write_enabled {
            let (input_tx, mut input_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
            let writer = Arc::clone(&writer);
            std::thread::spawn(move || {
                while let Some(bytes) = input_rx.blocking_recv() {
                    let Ok(mut writer) = writer.lock() else { break };
                    if writer.write_all(&bytes).is_err() {
                        break;
                    }
                    let _ = writer.flush();
                }
            });
            Some(input_tx)
        } else {
            None
        };

        if interactive {
            term::enter_raw();
            std::thread::spawn(move || {
                let mut stdin = std::io::stdin();
                let mut buf = [0u8; 4096];
                while let Ok(read) = stdin.read(&mut buf) {
                    if read == 0 {
                        break;
                    }
                    let Ok(mut writer) = writer.lock() else { break };
                    if writer.write_all(&buf[..read]).is_err() {
                        break;
                    }
                    let _ = writer.flush();
                }
            });
        }
        remote_tx
    } else {
        None
    };

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

    Ok((out_rx, input_tx, master))
}

/// Own a viewer's reverse stream. A write viewer's input frames are forwarded
/// into the PTY writer channel; its clean FIN (e.g. piped-stdin EOF) just ends
/// the task — the viewer keeps watching. Any protocol fault — a bad frame from
/// a write viewer, or *any* byte from a read-only viewer (a correct client
/// writes nothing after the secret) — closes the connection, which the hub's
/// next broadcast write turns into an eviction.
fn spawn_viewer_reader(
    conn: Connection,
    mut recv: RecvStream,
    input_tx: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
) {
    tokio::spawn(async move {
        let Some(input_tx) = input_tx else {
            let mut buf = [0u8; 64];
            if matches!(recv.read(&mut buf).await, Ok(Some(_))) {
                conn.close(2u32.into(), b"read-only viewer sent data");
            }
            return;
        };
        loop {
            match super::read_input_frame(&mut recv).await {
                Ok(Some(bytes)) => {
                    if input_tx.send(bytes).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    conn.close(2u32.into(), b"protocol violation");
                    break;
                }
            }
        }
    });
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
/// (it went away) or timed out (it stopped reading). The timeout bounds how long
/// one stalled viewer can hold up the hub — and with it the sharer's own shell.
async fn broadcast(viewers: &mut Vec<Viewer>, frame: &[u8]) {
    if viewers.is_empty() {
        return;
    }
    let results =
        futures_util::future::join_all(viewers.iter_mut().map(|viewer| {
            tokio::time::timeout(VIEWER_WRITE_TIMEOUT, viewer.send.write_all(frame))
        }))
        .await;
    let mut keep = results.into_iter().map(|write| matches!(write, Ok(Ok(()))));
    viewers.retain(|viewer| {
        if keep.next().unwrap_or(true) {
            return true;
        }
        // A timed-out write was cancelled mid-frame, so the stream is desynced;
        // the viewer cannot be kept.
        viewer.conn.close(0u32.into(), b"viewer too slow");
        false
    });
}

/// Accept one incoming connection, take its bi-stream, and verify the bearer
/// token (the viewer opens the bi-stream and writes the token first). Which
/// token matched decides the capability: the returned bool is "may write". The
/// comparison is constant-time so a passworded token can't be recovered by
/// timing. On a passworded session a mismatch is a wrong password; the raw
/// secret without the password computes no matching token.
///
/// # Errors
/// A failed handshake or a token matching neither ticket (closed with code 1).
async fn authenticate(
    incoming: Incoming,
    secrets: Secrets,
) -> Result<(Connection, SendStream, RecvStream, bool)> {
    let conn = incoming.await.context("incoming connection failed")?;
    let (send, mut recv) = conn.accept_bi().await.context("accept_bi failed")?;
    let mut got = [0u8; SECRET_LEN];
    if recv.read_exact(&mut got).await.is_err() {
        conn.close(1u32.into(), b"bad secret");
        bail!("peer presented a bad secret");
    }
    let can_write = if ct_eq(&got, &secrets.read) {
        false
    } else if secrets
        .write
        .is_some_and(|write_token| ct_eq(&got, &write_token))
    {
        true
    } else {
        conn.close(1u32.into(), b"bad secret");
        bail!("peer presented a bad secret");
    };
    Ok((conn, send, recv, can_write))
}

/// Build the command run inside the PTY: `--command` (a test/ops knob) runs via
/// `sh -c`; otherwise the sharer's `$SHELL` (falling back to `/bin/sh`). Inherits
/// the current working directory and ensures `TERM` is set.
fn build_command(command: Option<&str>, sh_prefix: &str) -> CommandBuilder {
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
    // A nested `ahsw sh` overrides this — the innermost session wins.
    cmd.env(super::ENV_SH, sh_prefix);
    cmd
}
