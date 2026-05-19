//! Thin binary shim. All CLI logic lives in the library
//! ([`agent_habilis_swarm::run_cli`]); `main` owns only process-level
//! concerns the library must not: tracing init and terminal echo.

use anyhow::Result;

/// Suppress `^C` echo in the terminal so ctrl-c exits cleanly.
#[expect(
    unsafe_code,
    reason = "libc termios FFI to clear ECHOCTL; no safe wrapper available"
)]
fn suppress_ctrl_c_echo() {
    #[cfg(unix)]
    unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(libc::STDIN_FILENO, &raw mut termios) == 0 {
            termios.c_lflag &= !libc::ECHOCTL;
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw const termios);
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // `noq_proto::connection=off`: iroh's multipath QUIC fork logs a
    // superseded-path PTO at ERROR. In public swarms every member
    // co-hosts the beacon under one shared `rendezvous_id`, so iroh
    // constantly opens/supersedes paths to it — expected, benign churn,
    // not a failure. Scoped to that one module; still overridable via
    // `RUST_LOG` (env is tried first). See AGENTS.md "Logging".
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(if cfg!(debug_assertions) {
            "info,noq_proto::connection=off"
        } else {
            "error,noq_proto::connection=off"
        })
    });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    suppress_ctrl_c_echo();
    agent_habilis_swarm::run_cli().await
}
