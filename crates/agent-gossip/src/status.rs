// Cargo-style status output for the operator-facing commands (`plug`, `unplug`,
// `doctor`) and for the `cargo task` dev runner — a right-aligned (12-col) bold
// verb + message.
//
// Lives here, not in the engine: it is pure presentation for *this* product's
// commands, and the engine renders nothing. `cargo task` shares it rather than
// forking a copy — both audiences want the same lines.
//
// Written through `anstream`, which resolves color support per stream at write
// time: a terminal gets ANSI, a pipe/file/`NO_COLOR` gets plain bytes. So the
// color never reaches an agent capturing stdout, and no caller needs a `--color`
// flag or a TTY check of its own.
//
// Stream split: stdout is the product, stderr is only for errors. A status line
// IS the product of `plug`/`unplug` (they print nothing else), so it goes to
// stdout and survives `agent-gossip plug > roster.txt`. Only `warn`/`error` —
// diagnostics, not output — go to stderr.

use std::path::{Path, PathBuf};

use anstyle::{AnsiColor, Style};

/// A status line: a right-aligned-12 bold-green `verb` then `msg`, on stdout.
/// An empty `msg` prints the verb alone, no trailing space.
pub fn status(verb: &str, msg: &str) {
    line(AnsiColor::Green, verb, msg);
}

/// Like [`status`] but bold-**yellow** — for "not set up" / "out of date" rows.
pub fn status_warn(verb: &str, msg: &str) {
    line(AnsiColor::Yellow, verb, msg);
}

fn line(color: AnsiColor, verb: &str, msg: &str) {
    let style = Style::new().fg_color(Some(color.into())).bold();
    if msg.is_empty() {
        anstream::println!("{style}{verb:>12}{style:#}");
    } else {
        anstream::println!("{style}{verb:>12}{style:#} {msg}");
    }
}

/// A cargo-style `warning: {msg}` line (bold-yellow `warning`), on stderr.
pub fn warn(msg: &str) {
    let style = Style::new().fg_color(Some(AnsiColor::Yellow.into())).bold();
    anstream::eprintln!("{style}warning{style:#}: {msg}");
}

/// A cargo-style `error: {msg}` line (bold-red `error`), on stderr.
pub fn error(msg: &str) {
    let style = Style::new().fg_color(Some(AnsiColor::Red.into())).bold();
    anstream::eprintln!("{style}error{style:#}: {msg}");
}

/// A path for display: `$HOME` collapsed to `~`, else the full path.
#[must_use]
pub fn home_path(path: &Path) -> String {
    #[cfg(unix)]
    let home = std::env::var_os("HOME");
    #[cfg(windows)]
    let home = windows_home();
    if let Some(home) = home
        && !home.is_empty()
        && let Ok(rest) = path.strip_prefix(PathBuf::from(home))
    {
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

/// The home of the user on Windows: `USERPROFILE`, which Windows sets and the
/// agents read, and else `HOME`. Git for Windows sets `HOME` to the same
/// folder; an MSYS2 or Cygwin `HOME` is another folder, which no agent reads.
#[cfg(windows)]
pub(crate) fn windows_home() -> Option<std::ffi::OsString> {
    std::env::var_os("USERPROFILE")
        .filter(|home| !home.is_empty())
        .or_else(|| std::env::var_os("HOME").filter(|home| !home.is_empty()))
}
