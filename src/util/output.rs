// cargo-style status output for the `plug`/`unplug` subcommands —
// a right-aligned (12-col) bold verb + message, written through `anstream`
// (which strips color when stderr isn't a terminal, so piped/agent output
// stays clean). Mirrors `../browse`'s `util::output`.
//
// CANONICAL SOURCE: the `cargo task` runner `include!`s this file verbatim
// (`tasks/src/util/output.rs`), so the binary and the dev-task runner share one
// definition with no crate dependency. Keep this file free of *inner* attributes
// (`//!` / `#![…]`) — `include!` rejects them. Each consumer puts an outer
// `#[expect(dead_code)]` on its `mod output` declaration (each uses a subset).

use std::path::{Path, PathBuf};

use anstyle::{AnsiColor, Style};

/// A green status line: a right-aligned-12 bold-green `verb` then `msg`.
pub(crate) fn status(verb: &str, msg: &str) {
    line(AnsiColor::Green, verb, msg);
}

/// Like [`status`] but bold-**yellow** — for "not set up" / "out of date" rows.
pub(crate) fn status_warn(verb: &str, msg: &str) {
    line(AnsiColor::Yellow, verb, msg);
}

fn line(color: AnsiColor, verb: &str, msg: &str) {
    let style = Style::new().fg_color(Some(color.into())).bold();
    anstream::eprintln!("{style}{verb:>12}{style:#} {msg}");
}

/// A cargo-style `warning: {msg}` line (bold-yellow `warning`).
pub(crate) fn warn(msg: &str) {
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
pub(crate) fn home_path(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME")
        && !home.is_empty()
        && let Ok(rest) = path.strip_prefix(PathBuf::from(home))
    {
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}
