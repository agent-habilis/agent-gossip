use std::io::IsTerminal;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use iroh::Endpoint;
use iroh::endpoint::{Connection, ConnectionError, RecvStream, SendStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;

use crate::lookup::{add_peer_addr, build_participant_endpoint};

use super::ticket::ShTicket;
use super::{Frame, SH_ALPN, encode_input, read_frame, term};

/// How long to keep retrying the dial while the producer's address propagates
/// (mDNS is instant on a LAN; the DHT fallback can take tens of seconds).
const DISCOVERY_DEADLINE: Duration = Duration::from_secs(90);
const RETRY_DELAY: Duration = Duration::from_secs(3);

/// Glyph painted into the margin around a source smaller than the viewer.
const FILL_CHAR: char = '·';
/// Minimum spacing between margin repaints during a `Data` burst — the fill only
/// needs refreshing when the source disturbs it, and never faster than the eye.
const BACKDROP_MIN_INTERVAL: Duration = Duration::from_millis(80);

/// Redeem `ticket` and render the peer's terminal until the sharer ends the
/// shell (stream FIN) or the viewer quits (Ctrl-C / `q` on a read ticket,
/// `Enter ~ .` on a write ticket).
///
/// # Errors
/// A malformed ticket, an unreachable producer, or a fatal stream I/O error.
pub(crate) async fn connect(ticket: &str) -> Result<()> {
    let ticket = ShTicket::decode(ticket)?;
    let endpoint = build_participant_endpoint(&ticket.lookups).await?;
    let result = view(&endpoint, &ticket).await;
    match result {
        // The session ended and the connection is closed; exit now rather than
        // await the slow `endpoint.close()` teardown.
        Ok(()) => std::process::exit(0),
        Err(error) => {
            endpoint.close().await;
            // An in-flight blocking stdin read (quit scanner / input forwarder)
            // would wedge the runtime's shutdown until a keypress — report and
            // exit directly instead of returning through main.
            crate::util::output::error(&format!("{error:#}"));
            std::process::exit(1);
        }
    }
}

/// Dial the producer (retrying while its address propagates), open the bi-stream,
/// and present the bearer secret.
async fn dial_and_handshake(
    endpoint: &Endpoint,
    ticket: &ShTicket,
) -> Result<(Connection, SendStream, RecvStream)> {
    add_peer_addr(endpoint, ticket.addr.clone())?;
    let start = Instant::now();
    let conn = loop {
        match endpoint.connect(ticket.addr.clone(), SH_ALPN).await {
            Ok(conn) => break conn,
            Err(error) if start.elapsed() < DISCOVERY_DEADLINE => {
                tracing::warn!(%error, "connect failed; retrying");
                tokio::time::sleep(RETRY_DELAY).await;
            }
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "could not reach the shell producer: {error}"
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

async fn view(endpoint: &Endpoint, ticket: &ShTicket) -> Result<()> {
    let (conn, send, recv) = dial_and_handshake(endpoint, ticket).await?;
    let tty = std::io::stdout().is_terminal();
    // Keyboard forwarding follows *stdin*: even with stdout redirected, a write
    // viewer typing on a tty needs raw mode (chords intact, no local SIGINT) and
    // the escape scanner.
    let forwards_tty = ticket.write && std::io::stdin().is_terminal();
    if forwards_tty {
        // On stderr: stdout is the rendered product (it may be redirected), and
        // on a tty the alternate screen would hide the hint anyway until detach.
        crate::util::output::status(
            "Interactive",
            "your keys reach the shell — detach with Enter ~ .",
        );
    }
    if tty {
        term::enter_alt_screen();
    }
    if tty || forwards_tty {
        term::enter_raw();
    }
    let (mut view_cols, mut view_rows) = if tty {
        term::size()
    } else {
        (u16::MAX, u16::MAX)
    };
    let mut stdout = tokio::io::stdout();
    // Source dimensions, learned from the first `Resize` (sent on attach).
    let mut src: Option<(u16, u16)> = None;
    let mut last_paint = Instant::now();
    // The viewer's own terminal resizing must re-bound the scroll region and
    // re-fill the margin; ignored on the non-tty path.
    let mut winch = if tty {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change()).ok()
    } else {
        None
    };

    // Raw mode swallows SIGINT, so the viewer's keyboard is watched either way:
    // a write ticket forwards it to the shell (detach via `Enter ~ .`); a read
    // ticket only scans it for a quit key.
    let quit = Arc::new(Notify::new());
    let _held_send = if ticket.write {
        spawn_input_forwarder(send, forwards_tty, quit.clone());
        None
    } else {
        if tty {
            let quit = quit.clone();
            tokio::spawn(async move {
                let mut stdin = tokio::io::stdin();
                let mut byte = [0u8; 1];
                loop {
                    match stdin.read(&mut byte).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) if byte[0] == 0x03 || byte[0] == b'q' || byte[0] == b'Q' => {
                            quit.notify_one();
                            break;
                        }
                        Ok(_) => {}
                    }
                }
            });
        }
        // Held open (never written) to keep the authenticated stream alive.
        Some(send)
    };

    // `read_frame` is not cancellation-safe (multiple reads per frame), so it
    // runs in its own task and the select below consumes a channel — a SIGWINCH
    // or quit landing mid-frame can no longer desync the stream.
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel::<Result<Option<Frame>>>(16);
    tokio::spawn(async move {
        let mut recv = recv;
        loop {
            let item = read_frame(&mut recv).await;
            let done = matches!(item, Ok(None) | Err(_));
            if frame_tx.send(item).await.is_err() || done {
                break;
            }
        }
    });

    let outcome = loop {
        tokio::select! {
            frame = frame_rx.recv() => {
                match frame {
                    None | Some(Ok(None)) => break session_end(&conn),
                    Some(Ok(Some(Frame::Data(bytes)))) => {
                        if let Err(error) = stdout.write_all(&bytes).await {
                            break Err(anyhow::Error::new(error).context("writing to stdout failed"));
                        }
                        // The source may have wiped the margin (e.g. a clear); re-fill
                        // it, throttled — the fill is only disturbed by `Data`.
                        if tty && last_paint.elapsed() >= BACKDROP_MIN_INTERVAL {
                            paint_backdrop(&mut stdout, src, view_cols, view_rows).await;
                            last_paint = Instant::now();
                        } else {
                            let _ = stdout.flush().await;
                        }
                    }
                    Some(Ok(Some(Frame::Resize { cols, rows }))) => {
                        src = Some((cols, rows));
                        if tty {
                            set_scroll_region(&mut stdout, rows, view_rows).await;
                            paint_backdrop(&mut stdout, src, view_cols, view_rows).await;
                            last_paint = Instant::now();
                        }
                    }
                    Some(Err(error)) => break Err(error),
                }
            }
            Some(()) = maybe_winch(&mut winch) => {
                (view_cols, view_rows) = term::size();
                if let Some((_, src_rows)) = src {
                    set_scroll_region(&mut stdout, src_rows, view_rows).await;
                }
                paint_backdrop(&mut stdout, src, view_cols, view_rows).await;
                last_paint = Instant::now();
            }
            () = quit.notified() => break Ok(()),
        }
    };

    if tty {
        let _ = stdout.flush().await;
        term::leave_alt_screen();
    }
    if tty || forwards_tty {
        term::restore();
    }
    conn.close(0u32.into(), b"bye");
    outcome
}

/// The producer's stream ended — decide whether that was the shell finishing or
/// this viewer being rejected/evicted, which must not exit 0 silently.
fn session_end(conn: &Connection) -> Result<()> {
    let Some(reason) = conn.close_reason() else {
        return Ok(());
    };
    if let ConnectionError::ApplicationClosed(app) = &reason {
        let code = app.error_code.into_inner();
        if code == 0 {
            return Ok(());
        }
        bail!(
            "the producer ended the session: {} (code {code})",
            String::from_utf8_lossy(&app.reason)
        );
    }
    if matches!(reason, ConnectionError::LocallyClosed) {
        return Ok(());
    }
    Err(anyhow::anyhow!("connection to the producer lost: {reason}"))
}

/// Forward the viewer's stdin to the producer as input frames. On a tty the
/// chunks pass through the `Enter ~ .` escape scanner and a detach fires `quit`;
/// piped stdin is forwarded verbatim (the scripted-input path) and its EOF just
/// FINs the send half — the viewer keeps watching.
fn spawn_input_forwarder(mut send: SendStream, escape: bool, quit: Arc<Notify>) {
    tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut buf = [0u8; 4096];
        let mut state = EscapeState::default();
        loop {
            match stdin.read(&mut buf).await {
                Ok(0) | Err(_) => {
                    // FIN, not drop: a reset mid-session would read as a fault.
                    let _ = send.finish();
                    break;
                }
                Ok(read) => {
                    let (forward, detach) = if escape {
                        scan_input(&mut state, &buf[..read])
                    } else {
                        (buf[..read].to_vec(), false)
                    };
                    if !forward.is_empty() && send.write_all(&encode_input(&forward)).await.is_err()
                    {
                        break;
                    }
                    if detach {
                        let _ = send.finish();
                        quit.notify_one();
                        break;
                    }
                }
            }
        }
    });
}

/// Scanner state for the ssh-style `Enter ~ .` detach sequence. Only a `~` at
/// the start of a line arms it, so typed tildes elsewhere pass through; the
/// state persists across `read()` chunks (the `~` and `.` may arrive apart).
struct EscapeState {
    saw_tilde: bool,
    at_line_start: bool,
}

impl Default for EscapeState {
    fn default() -> Self {
        // Start-of-session counts as a line start, so `~.` as the first keys works.
        Self {
            saw_tilde: false,
            at_line_start: true,
        }
    }
}

/// Run `chunk` through the escape scanner: returns the bytes to forward and
/// whether a detach (`~.` at line start) was seen. `~~` at line start forwards
/// one literal `~`; `~` followed by anything else forwards both bytes.
fn scan_input(state: &mut EscapeState, chunk: &[u8]) -> (Vec<u8>, bool) {
    let mut forward = Vec::with_capacity(chunk.len());
    for &byte in chunk {
        if state.saw_tilde {
            state.saw_tilde = false;
            if byte == b'.' {
                return (forward, true);
            }
            if byte != b'~' {
                forward.push(b'~');
            }
            forward.push(byte);
            state.at_line_start = byte == b'\r' || byte == b'\n';
        } else if state.at_line_start && byte == b'~' {
            state.saw_tilde = true;
        } else {
            forward.push(byte);
            state.at_line_start = byte == b'\r' || byte == b'\n';
        }
    }
    (forward, false)
}

/// Cap the scroll region to what fits (`min(src_rows, view_rows)`), so a source
/// taller than the viewer doesn't scroll past the viewer's screen.
async fn set_scroll_region(stdout: &mut tokio::io::Stdout, src_rows: u16, view_rows: u16) {
    let bound = src_rows.min(view_rows).max(1);
    let _ = stdout
        .write_all(format!("\x1b[1;{bound}r").as_bytes())
        .await;
    let _ = stdout.flush().await;
}

/// Paint the faint dotted margin around the source's box (once the source size
/// is known), then flush. A no-op when there is no gap.
async fn paint_backdrop(
    stdout: &mut tokio::io::Stdout,
    src: Option<(u16, u16)>,
    view_cols: u16,
    view_rows: u16,
) {
    if let Some((src_cols, src_rows)) = src {
        let seq = backdrop_sequence(src_cols, src_rows, view_cols, view_rows);
        if !seq.is_empty() {
            let _ = stdout.write_all(seq.as_bytes()).await;
        }
    }
    let _ = stdout.flush().await;
}

/// Build the escape sequence that fills the margin outside the source's
/// `src_cols × src_rows` box with a faint [`FILL_CHAR`], within the viewer's
/// `view_cols × view_rows` screen — the right gap for each shared row and the
/// full-width bottom gap below. Empty when the source is at least as large as the
/// viewer in both dimensions. Wrapped in DECSC/DECRC (`\x1b7`/`\x1b8`) so the
/// live cursor and pen are preserved.
fn backdrop_sequence(src_cols: u16, src_rows: u16, view_cols: u16, view_rows: u16) -> String {
    use std::fmt::Write;

    let right = view_cols.saturating_sub(src_cols);
    let bottom = view_rows.saturating_sub(src_rows);
    if right == 0 && bottom == 0 {
        return String::new();
    }
    let mut out = String::from("\x1b7\x1b[2m"); // save cursor+attrs, faint
    if right > 0 {
        let fill = FILL_CHAR.to_string().repeat(usize::from(right));
        let start_col = u32::from(src_cols) + 1;
        for row in 1..=u32::from(src_rows.min(view_rows)) {
            let _ = write!(out, "\x1b[{row};{start_col}H{fill}");
        }
    }
    if bottom > 0 {
        let fill = FILL_CHAR.to_string().repeat(usize::from(view_cols));
        for row in (u32::from(src_rows) + 1)..=u32::from(view_rows) {
            let _ = write!(out, "\x1b[{row};1H{fill}");
        }
    }
    out.push_str("\x1b[0m\x1b8"); // reset SGR, restore cursor+attrs
    out
}

/// Await the viewer's next `SIGWINCH` if a handler is installed, else never
/// resolve — so the `select!` arm is inert on the non-tty path.
async fn maybe_winch(sig: &mut Option<tokio::signal::unix::Signal>) -> Option<()> {
    match sig {
        Some(sig) => sig.recv().await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::{EscapeState, FILL_CHAR, backdrop_sequence, scan_input};

    #[test]
    fn tilde_dot_at_session_start_detaches() {
        let mut state = EscapeState::default();
        assert_eq!(scan_input(&mut state, b"~."), (vec![], true));
    }

    #[test]
    fn tilde_dot_after_enter_detaches() {
        let mut state = EscapeState::default();
        let (forward, detach) = scan_input(&mut state, b"ls\r~.");
        assert_eq!(forward, b"ls\r");
        assert!(detach);
    }

    #[test]
    fn double_tilde_forwards_one_literal_tilde() {
        let mut state = EscapeState::default();
        assert_eq!(scan_input(&mut state, b"~~"), (b"~".to_vec(), false));
    }

    #[test]
    fn tilde_then_other_forwards_both() {
        let mut state = EscapeState::default();
        assert_eq!(scan_input(&mut state, b"~x"), (b"~x".to_vec(), false));
    }

    #[test]
    fn mid_line_tilde_dot_forwards_verbatim() {
        let mut state = EscapeState::default();
        assert_eq!(scan_input(&mut state, b"a~."), (b"a~.".to_vec(), false));
    }

    #[test]
    fn escape_state_persists_across_chunks() {
        // `\r`, `~`, `.` arriving in three separate reads still detach.
        let mut state = EscapeState::default();
        assert_eq!(scan_input(&mut state, b"a\r"), (b"a\r".to_vec(), false));
        assert_eq!(scan_input(&mut state, b"~"), (vec![], false));
        assert_eq!(scan_input(&mut state, b"."), (vec![], true));
    }

    #[test]
    fn read_only_quit_keys_are_forwarded_in_write_mode() {
        // `q` and Ctrl-C belong to the shell on a write ticket.
        let mut state = EscapeState::default();
        assert_eq!(scan_input(&mut state, b"q\x03"), (b"q\x03".to_vec(), false));
    }

    #[test]
    fn no_gap_produces_nothing() {
        // Exact fit, and a viewer smaller than the source, both yield no fill.
        assert_eq!(backdrop_sequence(80, 24, 80, 24), "");
        assert_eq!(backdrop_sequence(80, 24, 70, 20), "");
    }

    #[test]
    fn wider_and_taller_viewer_fills_the_gap() {
        let seq = backdrop_sequence(80, 24, 100, 30);
        assert!(seq.starts_with('\u{1b}'), "opens with an escape");
        assert!(
            seq.contains("\x1b7") && seq.ends_with("\x1b8"),
            "saves+restores cursor"
        );
        assert!(seq.contains("\x1b[2m"), "faint");
        assert!(seq.contains("\x1b[0m"), "reset");
        // Right gap: (100-80) cols over 24 rows; bottom gap: (30-24) rows × 100 cols.
        let dots = seq.matches(FILL_CHAR).count();
        assert_eq!(dots, 20 * 24 + 6 * 100);
    }
}
