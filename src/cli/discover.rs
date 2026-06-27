//! The `discover` subcommand: browse a directory's live swarms.
//!
//! Human + TTY renders a live arrow-key picker that hands off to `join`
//! on selection; `--no-interactive` / `--output json` streams
//! `swarm_found`/`swarm_lost` JSON lines for an agent to act on. The pure
//! directory primitives live in [`crate::directory`]; the live consumer
//! in [`crate::embed::Directory`]; this file is just the CLI command +
//! terminal UI.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::signal::unix::{Signal, SignalKind, signal};

use crate::embed::{Directory, DirectoryEvent, SwarmListing};
use crate::resolver::JoinTarget;

use super::args::{DiscoverOpts, OutputFormat};
use super::join;

/// Browse a directory. Human mode renders a live picker that
/// hands off to `join` on selection; `--no-interactive` / `--output
/// json` streams `swarm_found`/`swarm_lost` JSON lines instead (the
/// agent picks and joins by id itself).
pub(super) async fn discover(opts: DiscoverOpts) -> Result<()> {
    let directory_label = opts
        .directory
        .as_ref()
        .map_or_else(|| "global".to_owned(), |name| name.as_str().to_owned());
    // Reach the directory over the chosen lookups (default all-on). Must
    // match the lookups the advertiser used — an mDNS-only advertiser is
    // found only by an mDNS-only `discover`.
    let mut discoverer = Directory::open(
        opts.directory.map(|name| name.as_str().to_owned()),
        opts.lookups.to_set(),
    )
    .await?;
    // Route the directory session's logs to its per-member file (same as
    // create/join) so the picker and JSON stream aren't drowned in INFO
    // lines on stderr.
    if let Some((swarm, nickname)) = discoverer.session_identity() {
        crate::logging::attach(swarm, nickname);
    }
    let mut events = discoverer
        .events()
        .expect("discoverer events receiver is available exactly once");

    // Human + a real terminal ⇒ the live arrow-key picker; otherwise
    // (agent / piped) stream JSON changes.
    let want_picker = !opts.shared.no_interactive && opts.shared.output == OutputFormat::Human;
    if want_picker {
        match run_picker(&directory_label, &discoverer, &mut events).await {
            PickerOutcome::Selected(id) => {
                let _ = discoverer.close().await;
                // Leave the directory's log file behind so `join` opens
                // the joined swarm's own file (with its setup logs) rather
                // than appending to the directory session's.
                crate::logging::detach();
                let target = id
                    .parse::<JoinTarget>()
                    .expect("a discovered swarm id is a valid join target");
                return join(target, None, opts.shared).await;
            }
            PickerOutcome::Quit => {
                let _ = discoverer.close().await;
                return Ok(());
            }
            // No TTY for raw mode — fall through to the streaming view.
            PickerOutcome::Unsupported => {}
        }
    }

    // Agent / non-TTY mode: one JSON line per directory change until
    // interrupted (SIGINT or SIGTERM — see `interrupted`).
    let mut sigterm = sigterm_stream();
    loop {
        tokio::select! {
            () = interrupted(&mut sigterm) => break,
            received = events.recv() => match received {
                Some(change) => println!("{}", discover_event_json(&change)),
                None => break,
            },
        }
    }
    let _ = discoverer.close().await;
    Ok(())
}

/// Resolves on SIGINT (ctrl-c) or SIGTERM — the signals a supervisor or
/// the `/swarm:discover` skill uses to stop a foreground discover. The
/// embed directory session registers its own SIGTERM handler (which
/// suppresses the OS default-terminate), so without listening for it here
/// a plain `kill` would hang the stream / picker loop.
async fn interrupted(sigterm: &mut Signal) {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm.recv() => {}
    }
}

/// A SIGTERM stream (the project is Unix-only). Held across a loop and
/// polled via [`interrupted`].
fn sigterm_stream() -> Signal {
    signal(SignalKind::terminate()).expect("register SIGTERM handler")
}

/// One directory change as a JSON line for `ahsw discover --output json`.
/// `Found`/`Updated` both surface as `swarm_found` (upsert semantics —
/// the agent treats a re-ad as a refresh); a departure is `swarm_lost`.
fn discover_event_json(event: &DirectoryEvent) -> String {
    let value = match event {
        DirectoryEvent::Found(listing) | DirectoryEvent::Updated(listing) => serde_json::json!({
            "event": "swarm_found",
            "swarm": listing.swarm.as_str(),
            "name": listing.name,
            "mode": if listing.public { "public" } else { "private" },
            "peers": listing.peers,
        }),
        DirectoryEvent::Lost(swarm) => serde_json::json!({
            "event": "swarm_lost",
            "swarm": swarm.as_str(),
        }),
    };
    value.to_string()
}

/// What the interactive picker resolved to.
enum PickerOutcome {
    /// A swarm was chosen — carries its full `ahsw…` id.
    Selected(String),
    /// User quit (`q` / esc / ctrl-c) or the directory closed.
    Quit,
    /// Raw terminal mode is unavailable (stdin isn't a TTY); the caller
    /// should fall back to the streaming view.
    Unsupported,
}

/// A keypress the picker acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Up,
    Down,
    Enter,
    Quit,
}

/// TTY controls for the inline picker (not color — always on since the
/// picker only runs under a TTY). The picker never clears the screen: it
/// redraws **in place**, overwriting only its own lines. Swarm-name color
/// reuses the shared `output::style` constants, gated on `NO_COLOR`.
///
/// `DISABLE_WRAP`/`ENABLE_WRAP` toggle the terminal's autowrap for the
/// picker's lifetime: with it off, every logical line is exactly one
/// physical row (over-wide ids are hard-cut at the right margin by the
/// terminal, not by us), so the move-cursor-up redraw math stays exact.
/// `ERASE_LINE` erases from the cursor to the end of the *current line*
/// only — never the whole screen (`\x1b[2J`) or to end-of-display
/// (`\x1b[J`), so scrollback above the picker is untouched.
const DISABLE_WRAP: &str = "\x1b[?7l";
const ENABLE_WRAP: &str = "\x1b[?7h";
const ERASE_LINE: &str = "\x1b[K";

/// Run the live arrow-key picker until the user selects/quits or the
/// directory closes. Redraws on every directory change; `↑`/`↓` (and
/// `j`/`k`) move, `enter` joins the highlighted swarm, `q`/esc/ctrl-c
/// quit. Returns [`PickerOutcome::Unsupported`] when stdin isn't a TTY.
async fn run_picker(
    directory_label: &str,
    discoverer: &Directory,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<DirectoryEvent>,
) -> PickerOutcome {
    let Some(raw) = RawMode::enable() else {
        return PickerOutcome::Unsupported;
    };
    // Turn off autowrap so over-wide listing lines stay one physical row
    // each (the terminal hard-cuts them at the margin); restored on exit.
    print_str(DISABLE_WRAP);
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (mut key_rx, key_handle) = spawn_key_reader(Arc::clone(&stop));

    let mut selected: usize = 0;
    // Lines drawn by the previous `render_picker`, so the next render can
    // move the cursor back up to the top of its own block and overwrite it
    // in place. 0 ⇒ nothing drawn yet (first render starts in place).
    let mut prev_lines: usize = 0;
    // Trailing-dot count (1..=3) for the empty-state "waiting for swarms"
    // animation, advanced by the `anim` tick below.
    let mut dots: usize = 3;
    // Cached listing set — refreshed only on a directory event (which is
    // the only thing that changes it). A keypress just moves `selected`,
    // so it reuses this Vec instead of re-cloning + re-sorting the whole
    // directory on every arrow press.
    let mut listings = discoverer.snapshot();
    render_picker(directory_label, &listings, selected, dots, &mut prev_lines);

    // Drives the empty-state dot animation; no directory event fires while
    // we are waiting, so the redraw has to be self-clocked.
    let mut anim = tokio::time::interval(Duration::from_millis(400));
    // A `kill` (SIGTERM/SIGINT) quits the picker so its `Drop` restores
    // the tty; without this the terminal would be left in raw mode.
    let mut sigterm = sigterm_stream();

    let outcome = loop {
        tokio::select! {
            () = interrupted(&mut sigterm) => break PickerOutcome::Quit,
            key = key_rx.recv() => {
                let Some(key) = key else { break PickerOutcome::Quit };
                match key {
                    Key::Up => selected = selected.saturating_sub(1),
                    Key::Down if selected + 1 < listings.len() => selected += 1,
                    Key::Down => {}
                    Key::Enter => {
                        if let Some(listing) = listings.get(selected) {
                            break PickerOutcome::Selected(listing.swarm.as_str().to_owned());
                        }
                    }
                    Key::Quit => break PickerOutcome::Quit,
                }
                render_picker(directory_label, &listings, selected, dots, &mut prev_lines);
            }
            change = events.recv() => {
                if change.is_none() {
                    break PickerOutcome::Quit; // directory closed
                }
                listings = discoverer.snapshot();
                selected = selected.min(listings.len().saturating_sub(1));
                render_picker(directory_label, &listings, selected, dots, &mut prev_lines);
            }
            _ = anim.tick() => {
                // Animate only while waiting; a populated list is static,
                // so there is nothing to repaint once swarms appear.
                if listings.is_empty() {
                    dots = dots % 3 + 1;
                    render_picker(directory_label, &listings, selected, dots, &mut prev_lines);
                }
            }
        }
    };

    // Stop the reader *before* restoring the tty: it must exit while
    // still in VTIME-poll mode, or its next read would block on a line
    // and steal stdin from the `join` we hand off to.
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = key_handle.join();
    drop(raw);
    // Restore autowrap, then erase only the picker's own block so the
    // discover chrome leaves no trace: the `join` handoff (or the shell
    // prompt on quit) takes its place, while the `$ ahsw discover` prompt and
    // everything above stay put — i.e. selecting a swarm lands in the same
    // terminal state as a direct `ahsw join`.
    print_str(ENABLE_WRAP);
    erase_block(prev_lines);
    outcome
}

/// Redraw the picker **in place**, overwriting only its own lines (never
/// clearing the screen): lowercase chrome, the directory + each swarm name
/// in yellow, the full `ahsw…` id, peer count, and a local first-seen
/// timestamp (`YYYY-MM-DD HH:MM`). `selected` is the highlighted row.
///
/// `prev_lines` is the line count of the previous render: when non-zero the
/// cursor is moved back up to the top of that block (`\x1b[{n}F`) before
/// rewriting; on the first render (0) it starts at column 0 of the current
/// line, so everything already on screen is preserved. Every line ends with
/// `ERASE_LINE` to scrub leftovers from a longer prior render, and when the
/// block shrinks the surplus rows are blanked the same way — so no
/// screen/display clear is ever emitted. Output post-processing (ONLCR) is
/// left on, so `\n` still becomes CRLF in raw mode.
fn render_picker(
    directory_label: &str,
    listings: &[SwarmListing],
    selected: usize,
    dots: usize,
    prev_lines: &mut usize,
) {
    use crate::output::style;
    use std::fmt::Write as _;

    // Reuse the shared swarm-name color, gated on a TTY + `NO_COLOR`
    // like every other colored path. Empty strings when off.
    let (yellow, reset) = if crate::output::stdout_color() {
        (style::SWARM, style::RESET)
    } else {
        ("", "")
    };

    // Build the visible block as logical lines; `push_line` terminates each
    // with `ERASE_LINE` + `\n` and counts it.
    let mut out = String::new();
    let mut new_lines: usize = 0;
    let mut push_line = |buf: &mut String, line: &str| {
        buf.push_str(line);
        buf.push_str(ERASE_LINE);
        buf.push('\n');
        new_lines += 1;
    };

    push_line(
        &mut out,
        &format!("discovering {yellow}#{directory_label}{reset} directory"),
    );
    push_line(&mut out, "");
    if listings.is_empty() {
        push_line(&mut out, &format!("waiting for swarms{}", ".".repeat(dots)));
    } else {
        for (index, listing) in listings.iter().enumerate() {
            let marker = if index == selected { "❯" } else { " " };
            let bold = if index == selected && !yellow.is_empty() {
                style::BOLD
            } else {
                ""
            };
            push_line(
                &mut out,
                &format!(
                    "{marker} {bold}{yellow}#{}{reset}  {}  {}  {}",
                    listing.name,
                    listing.swarm.as_str(),
                    listing.peers,
                    crate::util::clock::local_datetime(listing.first_seen_unix),
                ),
            );
        }
    }
    push_line(&mut out, "");
    push_line(&mut out, "↑/↓ move · enter join · q quit");

    // Prefix: move to the top of the previous block (in place), or just to
    // column 0 of the current line on the first render.
    let mut framed = String::new();
    if *prev_lines > 0 {
        let _ = write!(framed, "\x1b[{}F", *prev_lines);
    } else {
        framed.push('\r');
    }
    framed.push_str(&out);
    // If the block shrank, blank the now-stale trailing rows, then return
    // the cursor to just below the new block so the next render lines up.
    if *prev_lines > new_lines {
        let surplus = *prev_lines - new_lines;
        for _ in 0..surplus {
            framed.push_str(ERASE_LINE);
            framed.push('\n');
        }
        let _ = write!(framed, "\x1b[{surplus}F");
    }
    *prev_lines = new_lines;
    print_str(&framed);
}

/// Erase the picker's own `lines` rows on the way out, leaving the cursor
/// at the block's top so a `join` handoff (or the shell prompt) continues
/// exactly where the picker began. Touches only the picker's lines — moves
/// to the top of the block (`\x1b[{lines}F`), blanks each row with
/// `ERASE_LINE`, then returns to the top — never the whole screen.
fn erase_block(lines: usize) {
    use std::fmt::Write as _;

    if lines == 0 {
        return;
    }
    let mut out = String::new();
    let _ = write!(out, "\x1b[{lines}F");
    for _ in 0..lines {
        out.push_str(ERASE_LINE);
        out.push('\n');
    }
    let _ = write!(out, "\x1b[{lines}F");
    print_str(&out);
}

/// Write a string straight to stdout and flush — the picker controls the
/// screen itself rather than going through `println!`.
fn print_str(text: &str) {
    use std::io::Write as _;
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

/// RAII raw-terminal guard for the picker: disables canonical mode,
/// echo, and signal generation (so ctrl-c arrives as a byte we handle),
/// and sets a ~100 ms read poll (`VMIN=0`/`VTIME=1`) so the reader
/// thread can observe its stop flag. Restores the original settings on
/// drop. `None` when stdin isn't a TTY.
struct RawMode {
    original: libc::termios,
}

impl RawMode {
    #[expect(
        unsafe_code,
        reason = "libc termios FFI to put the tty in raw mode for the discover picker; no safe wrapper"
    )]
    fn enable() -> Option<Self> {
        // SAFETY: a zeroed `termios` is a valid buffer for `tcgetattr` to
        // fill; all calls target the process's own stdin fd.
        unsafe {
            let mut termios: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &raw mut termios) != 0 {
                return None;
            }
            let original = termios;
            termios.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
            termios.c_cc[libc::VMIN] = 0;
            termios.c_cc[libc::VTIME] = 1;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw const termios) != 0 {
                return None;
            }
            Some(Self { original })
        }
    }
}

impl Drop for RawMode {
    #[expect(
        unsafe_code,
        reason = "libc termios FFI to restore the tty when the picker exits; no safe wrapper"
    )]
    fn drop(&mut self) {
        // SAFETY: restoring the exact `termios` captured in `enable`.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw const self.original);
        }
    }
}

/// Spawn a thread that reads stdin in raw mode and forwards parsed
/// [`Key`]s over a channel. It polls `stop` (via the `VTIME` read
/// timeout) and exits within ~100 ms once set — so it releases stdin
/// before the picker hands control to `join`.
fn spawn_key_reader(
    stop: Arc<std::sync::atomic::AtomicBool>,
) -> (
    tokio::sync::mpsc::UnboundedReceiver<Key>,
    std::thread::JoinHandle<()>,
) {
    let (key_tx, key_rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            match read_stdin(&mut buf) {
                Some(0) => {} // VTIME timeout, no input — re-check `stop`
                Some(count) => {
                    for key in parse_keys(&buf[..count]) {
                        if key_tx.send(key).is_err() {
                            return;
                        }
                    }
                }
                None => return, // read error
            }
        }
    });
    (key_rx, handle)
}

/// One raw `read(2)` from stdin. `Some(0)` on the `VTIME` poll timeout,
/// `Some(n)` with bytes read, `None` on error. Bypasses std's buffered
/// stdin so the `VMIN`/`VTIME` poll behaves as configured.
#[expect(
    unsafe_code,
    reason = "libc read(2) on STDIN_FILENO to honor the raw-mode VMIN/VTIME poll; std's buffered stdin would not"
)]
fn read_stdin(buf: &mut [u8]) -> Option<usize> {
    // SAFETY: `read` writes at most `buf.len()` bytes into the valid
    // mutable slice `buf`.
    let count = unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr().cast(), buf.len()) };
    usize::try_from(count).ok()
}

/// Parse a raw input chunk into [`Key`]s: arrow `↑`/`↓` (CSI `A`/`B`),
/// `enter`, `q`/esc/ctrl-c as quit, and vim `j`/`k`. Unknown bytes and
/// other escape sequences are ignored.
fn parse_keys(bytes: &[u8]) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            0x1b => {
                // CSI: ESC '[' final-byte. Recognize the arrows; skip
                // any other sequence. A lone ESC is a quit.
                if bytes.get(index + 1) == Some(&b'[') {
                    match bytes.get(index + 2) {
                        Some(b'A') => keys.push(Key::Up),
                        Some(b'B') => keys.push(Key::Down),
                        _ => {}
                    }
                    index += 3;
                    continue;
                }
                keys.push(Key::Quit);
            }
            b'\r' | b'\n' => keys.push(Key::Enter),
            b'q' | b'Q' | 0x03 => keys.push(Key::Quit),
            b'j' => keys.push(Key::Down),
            b'k' => keys.push(Key::Up),
            _ => {}
        }
        index += 1;
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::{Key, parse_keys};

    #[test]
    fn parse_keys_recognizes_arrows_enter_and_quit() {
        // Arrow keys arrive as a CSI sequence, usually in one read.
        assert_eq!(parse_keys(b"\x1b[A"), vec![Key::Up]);
        assert_eq!(parse_keys(b"\x1b[B"), vec![Key::Down]);
        // Enter (CR or LF), vim j/k, q, and ctrl-c.
        assert_eq!(parse_keys(b"\r"), vec![Key::Enter]);
        assert_eq!(parse_keys(b"\n"), vec![Key::Enter]);
        assert_eq!(parse_keys(b"j"), vec![Key::Down]);
        assert_eq!(parse_keys(b"k"), vec![Key::Up]);
        assert_eq!(parse_keys(b"q"), vec![Key::Quit]);
        assert_eq!(parse_keys(&[0x03]), vec![Key::Quit]);
        // A lone ESC is a quit; other CSI sequences (e.g. →) are ignored.
        assert_eq!(parse_keys(b"\x1b"), vec![Key::Quit]);
        assert_eq!(parse_keys(b"\x1b[C"), vec![]);
        // Unknown bytes ignored; multiple keys in one chunk all parse.
        assert_eq!(parse_keys(b"x\x1b[Bq"), vec![Key::Down, Key::Quit]);
    }
}
