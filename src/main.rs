//! Thin binary shim. All CLI logic lives in the library
//! ([`agent_gossip::run_cli`]); `main` owns only process-level
//! concerns the library must not: tracing init and terminal echo.

use anyhow::Result;

/// Original tty state, saved before `^C` echo is disabled so it can
/// be restored on exit.
#[cfg(unix)]
static ORIG_TERMIOS: std::sync::OnceLock<libc::termios> = std::sync::OnceLock::new();

/// Restore the saved tty state. Registered via libc `atexit`.
#[cfg(unix)]
#[expect(unsafe_code, reason = "libc termios FFI; no safe wrapper")]
extern "C" fn restore_ctrl_c_echo() {
    if let Some(orig) = ORIG_TERMIOS.get() {
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, orig);
        }
    }
}

/// Disable the tty's `^C` echo so ctrl-c exits cleanly, saving the
/// original and registering a libc `atexit` restore. `atexit` not
/// `Drop`: the daemon's ctrl-c / SIGTERM path exits via
/// `std::process::exit`, which runs C `atexit` handlers but skips
/// destructors — otherwise `agent-gossip` would leave the terminal with `^C`
/// echo off after it exits.
#[expect(
    unsafe_code,
    reason = "libc termios FFI to clear ECHOCTL and atexit-register the restore"
)]
fn suppress_ctrl_c_echo() {
    #[cfg(unix)]
    unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(libc::STDIN_FILENO, &raw mut termios) == 0 {
            if ORIG_TERMIOS.set(termios).is_ok() {
                let _ = libc::atexit(restore_ctrl_c_echo);
            }
            let mut quiet = termios;
            quiet.c_lflag &= !libc::ECHOCTL;
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw const quiet);
        }
    }
}

/// Heap-profiling allocator, only under `--features dhat-heap`. Tracks every
/// allocation so the profiler can attribute retained bytes by backtrace.
#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// Compact global allocator, Linux only. The fleet ships a static-musl binary,
/// and musl's built-in malloc holds a lot of resident memory; swapping in
/// mimalloc roughly halves the daemon's anonymous RSS (measured on the aarch64
/// Pi fleet). Not on macOS: Apple's libmalloc is already lean there, so mimalloc
/// is pure overhead (measured ~1.6 MB worse), and the OS keeps its default.
/// Yielded to dhat's allocator under the profiling feature.
#[cfg(all(not(feature = "dhat-heap"), target_os = "linux"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

// Single-threaded runtime: the daemon is I/O-bound (awaits sockets + timers),
// so a worker-per-core pool only spends RSS on idle thread stacks and per-thread
// malloc arenas. current_thread keeps one thread; spawn_blocking still offloads
// the rare blocking call to the blocking pool.
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    // Heap profiler (dhat-heap feature): writes `dhat-heap.json` to CWD when this
    // guard drops. The daemon's CLI quit path normally `process::exit`s (skipping
    // destructors) — under `dhat-heap` that exit is gated out (see
    // `daemon::event_loop`) so a `--no-interactive` daemon returns cleanly through
    // here and the profile flushes. SIGTERM/ctrl-c the churning daemon to dump it.
    #[cfg(feature = "dhat-heap")]
    let _dhat = dhat::Profiler::new_heap();

    // Tracing buffers until create/join resolve swarm+nick, then
    // flushes to the per-member file; else stderr. The filter + sink
    // both live in the crate's `logging` module.
    tracing_subscriber::fmt()
        .with_env_filter(agent_gossip::log_filter())
        .with_writer(agent_gossip::install_log_sink())
        .with_ansi(false)
        .init();
    suppress_ctrl_c_echo();
    let result = agent_gossip::run_cli().await;
    agent_gossip::flush_log_if_pending();
    result
}
