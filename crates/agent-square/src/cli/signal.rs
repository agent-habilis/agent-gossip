//! Signal helpers for the foreground streaming commands (`discover`,
//! `a2a discover`). The embed directory session registers its own SIGTERM
//! handler (which suppresses the OS default terminate), so a streaming loop
//! must listen for SIGINT/SIGTERM itself or a plain `kill` would hang it.

use tokio::signal::unix::{Signal, SignalKind, signal};

/// Resolves on SIGINT (ctrl-c) or SIGTERM — the signals a supervisor or a
/// skill uses to stop a foreground stream.
pub(super) async fn interrupted(sigterm: &mut Signal) {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm.recv() => {}
    }
}

/// A SIGTERM stream (the project is Unix-only). Held across a loop and polled
/// via [`interrupted`].
pub(super) fn sigterm_stream() -> Signal {
    signal(SignalKind::terminate()).expect("register SIGTERM handler")
}
