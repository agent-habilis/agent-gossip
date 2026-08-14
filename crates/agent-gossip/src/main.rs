//! Thin binary shim. All CLI logic lives in the library
//! ([`agent_gossip::run_cli`]); `main` owns only process-level
//! concerns the library must not: tracing init.

use anyhow::Result;

/// Heap-profiling allocator, only under `--features dhat-heap`. Tracks every
/// allocation so the profiler can attribute retained bytes by backtrace.
#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// Compact global allocator, Linux only. The roughly halved anonymous RSS that
/// justified it was measured on the aarch64 Pi fleet against musl's own malloc,
/// back when the fleet shipped a static-musl binary; it ships glibc now, so
/// treat that number as stale and re-measure before leaning on it. Not on macOS:
/// Apple's libmalloc is already lean there, so mimalloc is pure overhead
/// (measured ~1.6 MB worse), and the OS keeps its default. Yielded to dhat's
/// allocator under the profiling feature.
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
    // `daemon::event_loop`) so the daemon returns cleanly through here and the
    // profile flushes. SIGTERM/ctrl-c the churning daemon to dump it.
    #[cfg(feature = "dhat-heap")]
    let _dhat = dhat::Profiler::new_heap();

    // Tracing buffers until create/join resolve mesh+nick, then
    // flushes to the per-member file; else stderr. The filter + sink
    // both live in the crate's `logging` module.
    tracing_subscriber::fmt()
        .with_env_filter(agent_gossip::log_filter())
        .with_writer(agent_gossip::install_log_sink())
        .with_ansi(false)
        .init();
    let result = agent_gossip::run_cli().await;
    agent_gossip::flush_log_if_pending();
    result
}
