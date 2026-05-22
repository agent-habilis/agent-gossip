//! Thin binary shim. All CLI logic lives in the library
//! ([`agent_habilis_swarm::run_cli`]); `main` owns only process-level
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
/// destructors — otherwise `ahs` would leave the terminal with `^C`
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

/// Default tracing directives when `RUST_LOG` is unset (`RUST_LOG`
/// wins). Quiets benign `noq_proto::connection`; release also drops the
/// env-dependent `mainline::rpc` DHT-bootstrap ERROR; the `messages`
/// target is pinned on so it lands at any base level. See AGENTS.md.
///
/// Our own operational subsystems (gossip/discovery/beacon/lifecycle)
/// are pinned to `info` in BOTH profiles so the always-on log file
/// carries the connectivity/lifecycle story even in a release build
/// (whose `error` base would otherwise drop every diagnostic) — the
/// same rationale as the `messages=info` pin. tracing writes only to
/// the file sink; `--output json` (stdout) is a separate path, so this
/// never affects the event stream.
fn log_filter() -> tracing_subscriber::EnvFilter {
    use tracing_subscriber::EnvFilter;
    const SUBSYSTEMS: &str = "agent_habilis_swarm::gossip=info,\
        agent_habilis_swarm::discovery=info,\
        agent_habilis_swarm::beacon=info,\
        agent_habilis_swarm::lifecycle=info,\
        agent_habilis_swarm::messages=info";
    EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(if cfg!(debug_assertions) {
            format!("info,noq_proto::connection=off,{SUBSYSTEMS}")
        } else {
            format!("error,noq_proto::connection=off,mainline::rpc=off,{SUBSYSTEMS}")
        })
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    // Tracing buffers until create/join resolve swarm+nick, then
    // flushes to the per-member file; else stderr. See `logsink`.
    tracing_subscriber::fmt()
        .with_env_filter(log_filter())
        .with_writer(agent_habilis_swarm::install_log_sink())
        .with_ansi(false)
        .init();
    suppress_ctrl_c_echo();
    let result = agent_habilis_swarm::run_cli().await;
    agent_habilis_swarm::flush_log_if_pending();
    result
}
