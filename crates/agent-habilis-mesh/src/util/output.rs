// cargo-style status output for the `plug`/`unplug` subcommands —
// a right-aligned (12-col) verb + message. Plain text, no color: this is the
// agent-facing binary, whose human/color layer was removed.
//
// The `cargo task` dev runner deliberately does NOT share this file (it used to,
// via `include!`): it is read by a human scanning a build log, where color is
// what separates a failure from a progress line, so it keeps its own styled copy
// in `crates/tasks/src/util/output.rs`.

use std::path::{Path, PathBuf};

/// A status line: a right-aligned-12 `verb` then `msg`, on stderr.
pub fn status(verb: &str, msg: &str) {
    eprintln!("{verb:>12} {msg}");
}

/// Like [`status`] but on **stdout** (for commands whose status IS the product a
/// script may read). An empty `msg` prints the verb alone, no trailing space.
pub(crate) fn status_out(verb: &str, msg: &str) {
    if msg.is_empty() {
        println!("{verb:>12}");
    } else {
        println!("{verb:>12} {msg}");
    }
}

/// Like [`status`] — a distinct entry point for "not set up" / "out of date" rows.
pub fn status_warn(verb: &str, msg: &str) {
    eprintln!("{verb:>12} {msg}");
}

/// A cargo-style `warning: {msg}` line.
pub fn warn(msg: &str) {
    eprintln!("warning: {msg}");
}

/// A cargo-style `error: {msg}` line.
pub(crate) fn error(msg: &str) {
    eprintln!("error: {msg}");
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
