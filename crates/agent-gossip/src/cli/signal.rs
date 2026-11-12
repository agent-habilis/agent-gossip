//! Signal helpers for the foreground streaming commands (`discover`,
//! `a2a discover`). The embed directory session registers its own SIGTERM
//! handler (which suppresses the OS default terminate), so a streaming loop
//! must listen for SIGINT/SIGTERM itself or a plain `kill` would hang it.
//! On Windows there is no SIGTERM, and a stream stops on ctrl-c only.

#[cfg(unix)]
use tokio::signal::unix::{Signal, SignalKind, signal};

/// Resolves on SIGINT (ctrl-c) or SIGTERM — the signals a supervisor or a
/// skill uses to stop a foreground stream.
#[cfg(unix)]
pub(super) async fn interrupted(sigterm: &mut Signal) {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm.recv() => {}
    }
}

/// A SIGTERM stream. Held across a loop and polled via [`interrupted`].
#[cfg(unix)]
pub(super) fn sigterm_stream() -> Signal {
    signal(SignalKind::terminate()).expect("register SIGTERM handler")
}

/// Windows has no SIGTERM, so a foreground stream stops on ctrl-c only. The
/// type keeps the callers the same on every OS.
#[cfg(windows)]
#[derive(Debug)]
pub(super) struct Signal;

/// Resolves on ctrl-c.
#[cfg(windows)]
pub(super) async fn interrupted(_sigterm: &mut Signal) {
    let _ = tokio::signal::ctrl_c().await;
}

/// The placeholder of the SIGTERM stream of Unix.
#[cfg(windows)]
pub(super) fn sigterm_stream() -> Signal {
    Signal
}
