// cargo-style status output for the `plug`/`unplug` subcommands —
// a right-aligned (12-col) verb + message. Plain text, no color (this is
// agent-facing tooling; a human-only color layer would only ever be stripped
// for the piped/agent readers that are the norm here).
//
// CANONICAL SOURCE: the `cargo task` runner `include!`s this file verbatim
// (`tasks/src/util/output.rs`), so the binary and the dev-task runner share one
// definition with no crate dependency. Keep this file free of *inner* attributes
// (`//!` / `#![…]`) — `include!` rejects them. Each consumer puts an outer
// `#[expect(dead_code)]` on its `mod output` declaration (each uses a subset).

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
