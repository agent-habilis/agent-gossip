//! Thin binary shim. All CLI logic lives in the library
//! ([`agent_gossip::run_cli`]); `main` owns only process-level
//! concerns the library must not: tracing init.

use anyhow::Result;

/// Compact global allocator, Linux only. The roughly halved anonymous RSS that
/// justified it was measured on the aarch64 Pi fleet against musl's own malloc,
/// back when the fleet shipped a static-musl binary; it ships glibc now, so
/// treat that number as stale and re-measure before leaning on it. Not on macOS:
/// Apple's libmalloc is already lean there, so mimalloc is pure overhead
/// (measured ~1.6 MB worse), and the OS keeps its default.
#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

// Single-threaded runtime: the daemon is I/O-bound (awaits sockets + timers),
// so a worker-per-core pool only spends RSS on idle thread stacks and per-thread
// malloc arenas. current_thread keeps one thread; spawn_blocking still offloads
// the rare blocking call to the blocking pool.
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    // Tracing buffers until create/join resolve mesh+nick, then
    // flushes to the per-member file; else stderr. The filter + sink
    // both live in the crate's `logging` module.
    // A failed log write is dropped, not reported: the report is an
    // `eprintln!` to stderr, which on a full disk is a file that fails too,
    // and `eprintln!` then panics. Under `panic = "abort"` that killed the
    // daemon with exit 134, an empty stderr, and no farewell to its peers.
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(agent_gossip::log_filter())
        .with_writer(agent_gossip::install_log_sink());
    subscriber
        .with_ansi(false)
        .log_internal_errors(false)
        .init();
    let result = agent_gossip::run_cli().await;
    agent_gossip::flush_log_if_pending();
    Ok(result?)
}
