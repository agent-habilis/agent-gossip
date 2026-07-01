//! The shell viewer: redeem a ticket, dial the producer, and render its terminal
//! read-only. Frames are written verbatim to stdout (raw passthrough — no screen
//! model); a `Resize` bounds the scroll region to what fits. When the viewer's
//! terminal is larger than the source, the margin outside the source's box is
//! filled with a faint dotted backdrop so the shared screen reads as a framed
//! box. On a tty the viewer enters the alternate screen and raw mode and quits
//! on Ctrl-C / `q`.

use std::io::IsTerminal;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use iroh::Endpoint;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;

use crate::lookup::{add_peer_addr, build_participant_endpoint};

use super::ticket::ShTicket;
use super::{Frame, SH_ALPN, read_frame, term};

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
/// shell (stream FIN) or the viewer quits (Ctrl-C / `q`).
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
            Err(error)
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
                return Err(anyhow::anyhow!("could not reach the shell producer: {error}"));
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
    // `_send` is held open only to keep the authenticated stream alive.
    let (conn, _send, mut recv) = dial_and_handshake(endpoint, ticket).await?;
    let tty = std::io::stdout().is_terminal();
    if tty {
        term::enter_alt_screen();
        term::enter_raw();
    }
    let (mut view_cols, mut view_rows) = if tty { term::size() } else { (u16::MAX, u16::MAX) };
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

    // Raw mode swallows SIGINT, so watch the viewer's keyboard for a quit key.
    let quit = Arc::new(Notify::new());
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

    let outcome = loop {
        tokio::select! {
            frame = read_frame(&mut recv) => {
                match frame {
                    Ok(None) => break Ok(()),
                    Ok(Some(Frame::Data(bytes))) => {
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
                    Ok(Some(Frame::Resize { cols, rows })) => {
                        src = Some((cols, rows));
                        if tty {
                            set_scroll_region(&mut stdout, rows, view_rows).await;
                            paint_backdrop(&mut stdout, src, view_cols, view_rows).await;
                            last_paint = Instant::now();
                        }
                    }
                    Err(error) => break Err(error),
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
        term::restore();
    }
    conn.close(0u32.into(), b"bye");
    outcome
}

/// Cap the scroll region to what fits (`min(src_rows, view_rows)`), so a source
/// taller than the viewer doesn't scroll past the viewer's screen.
async fn set_scroll_region(stdout: &mut tokio::io::Stdout, src_rows: u16, view_rows: u16) {
    let bound = src_rows.min(view_rows).max(1);
    let _ = stdout.write_all(format!("\x1b[1;{bound}r").as_bytes()).await;
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
    use super::{FILL_CHAR, backdrop_sequence};

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
        assert!(seq.contains("\x1b7") && seq.ends_with("\x1b8"), "saves+restores cursor");
        assert!(seq.contains("\x1b[2m"), "faint");
        assert!(seq.contains("\x1b[0m"), "reset");
        // Right gap: (100-80) cols over 24 rows; bottom gap: (30-24) rows × 100 cols.
        let dots = seq.matches(FILL_CHAR).count();
        assert_eq!(dots, 20 * 24 + 6 * 100);
    }
}
