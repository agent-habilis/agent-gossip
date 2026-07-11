// Cargo-style status output for the `cargo task` dev runner — a right-aligned
// (12-col) bold verb + message, written through `anstream` (which strips color
// when the stream isn't a terminal).
//
// This is a DELIBERATE fork of the engine's `util::output`, not a copy by
// accident. The two surfaces have opposite audiences: the shipped `agent-square`
// binary is agent-facing and prints plain text (its human/color layer was
// removed), while `cargo task` is read by a human developer scanning a build
// log, where color is what separates a failure from a progress line. Sharing one
// definition would force one audience's choice on the other.

use std::path::{Path, PathBuf};

use anstyle::{AnsiColor, Style};

/// A green status line: a right-aligned-12 bold-green `verb` then `msg`.
pub fn status(verb: &str, msg: &str) {
    line(AnsiColor::Green, verb, msg);
}

/// Like [`status`] but on **stdout** (for commands whose status IS the product a
/// script may read). An empty `msg` prints the verb alone, no trailing space.
pub(crate) fn status_out(verb: &str, msg: &str) {
    let style = Style::new().fg_color(Some(AnsiColor::Green.into())).bold();
    if msg.is_empty() {
        anstream::println!("{style}{verb:>12}{style:#}");
    } else {
        anstream::println!("{style}{verb:>12}{style:#} {msg}");
    }
}

/// Like [`status`] but bold-**yellow** — for "not set up" / "out of date" rows.
pub fn status_warn(verb: &str, msg: &str) {
    line(AnsiColor::Yellow, verb, msg);
}

fn line(color: AnsiColor, verb: &str, msg: &str) {
    let style = Style::new().fg_color(Some(color.into())).bold();
    anstream::eprintln!("{style}{verb:>12}{style:#} {msg}");
}

/// A cargo-style `warning: {msg}` line (bold-yellow `warning`).
pub fn warn(msg: &str) {
    let style = Style::new().fg_color(Some(AnsiColor::Yellow.into())).bold();
    anstream::eprintln!("{style}warning{style:#}: {msg}");
}

/// A cargo-style `error: {msg}` line (bold-red `error`).
pub(crate) fn error(msg: &str) {
    let style = Style::new().fg_color(Some(AnsiColor::Red.into())).bold();
    anstream::eprintln!("{style}error{style:#}: {msg}");
}

/// A path for display: `$HOME` collapsed to `~`, else the full path.
#[must_use]
pub fn home_path(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME")
        && !home.is_empty()
        && let Ok(rest) = path.strip_prefix(PathBuf::from(home))
    {
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}
