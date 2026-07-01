//! Controlling-terminal helpers for `ahsw sh`: query the window size, switch the
//! tty to raw mode (so keystrokes reach the shared shell char-at-a-time instead
//! of being cooked by the outer terminal), and drive the alternate screen on the
//! viewer. Raw-mode state is saved once and restored via libc `atexit`, because
//! both the producer and viewer exit through `std::process::exit` (which runs C
//! `atexit` handlers but skips `Drop`) — otherwise a shared session would leave
//! the terminal in raw mode after it ends.

use std::io::Write;
use std::sync::OnceLock;

#[cfg(unix)]
static ORIG_TERMIOS: OnceLock<libc::termios> = OnceLock::new();

/// The controlling terminal's size as `(cols, rows)`, or `(80, 24)` when it
/// can't be queried (not a tty, or the ioctl failed).
pub(super) fn size() -> (u16, u16) {
    #[cfg(unix)]
    if let Some(sz) = query_size() {
        return sz;
    }
    (80, 24)
}

#[cfg(unix)]
#[expect(
    unsafe_code,
    reason = "ioctl(TIOCGWINSZ) reads the controlling tty's window size; no safe wrapper"
)]
fn query_size() -> Option<(u16, u16)> {
    // SAFETY: ioctl fills a zeroed POD `winsize` for stdout's fd; we read only
    // its scalar `ws_col`/`ws_row` fields.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &raw mut ws) != 0 {
            return None;
        }
        if ws.ws_col == 0 || ws.ws_row == 0 {
            return None;
        }
        Some((ws.ws_col, ws.ws_row))
    }
}

/// Switch stdin to raw mode, saving the original state on first call and
/// registering an `atexit` restore. No-op if stdin isn't a tty.
#[cfg(unix)]
#[expect(
    unsafe_code,
    reason = "termios FFI: save the tty state and switch stdin to raw mode"
)]
pub(super) fn enter_raw() {
    // SAFETY: tcgetattr/tcsetattr operate on stdin's fd with a zeroed POD
    // `termios`; cfmakeraw only mutates that local struct.
    unsafe {
        let mut current: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(libc::STDIN_FILENO, &raw mut current) != 0 {
            return;
        }
        if ORIG_TERMIOS.set(current).is_ok() {
            let _ = libc::atexit(restore_at_exit);
        }
        let mut raw_mode = current;
        libc::cfmakeraw(&raw mut raw_mode);
        libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw const raw_mode);
    }
}

/// Restore the saved tty state, if raw mode was ever entered.
#[cfg(unix)]
#[expect(unsafe_code, reason = "termios FFI: restore the saved tty state")]
pub(super) fn restore() {
    if let Some(orig) = ORIG_TERMIOS.get() {
        // SAFETY: tcsetattr with the exact `termios` previously read by tcgetattr.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, orig);
        }
    }
}

#[cfg(unix)]
extern "C" fn restore_at_exit() {
    restore();
}

#[cfg(not(unix))]
pub(super) fn enter_raw() {}

#[cfg(not(unix))]
pub(super) fn restore() {}

/// Enter the alternate screen buffer and clear it, so the viewer's mirror does
/// not scroll the sharer's terminal into the viewer's own scrollback.
pub(super) fn enter_alt_screen() {
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x1b[?1049h\x1b[2J\x1b[H");
    let _ = out.flush();
}

/// Reset the scroll region and leave the alternate screen, returning the viewer's
/// terminal to how it was before `sh connect`.
pub(super) fn leave_alt_screen() {
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x1b[r\x1b[?1049l");
    let _ = out.flush();
}
