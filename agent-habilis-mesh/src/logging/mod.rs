//! Developer-log plumbing: the tracing directive filter ([`log_filter`]),
//! the deferred per-member file sink ([`sink`]), and the per-message
//! [`messages`] logger on the `agent_square::messages` target.
//! `--output json` (stdout) is a separate path and is unaffected by
//! anything here.

pub mod messages;
mod sink;

pub use sink::LogSink;
pub use sink::{attach, detach, flush_pending_to_stderr, install};

/// Default tracing directives when `RUST_LOG` is unset (`RUST_LOG`
/// wins). Quiets benign `noq_proto::connection`; release also drops the
/// env-dependent `mainline::rpc` DHT-bootstrap ERROR; the `messages`
/// target is pinned on so it lands at any base level. See AGENTS.md.
///
/// Our own operational subsystems (gossip/lookup/beacon/lifecycle/directory)
/// are pinned to `info` in BOTH profiles so the always-on log file
/// carries the connectivity/lifecycle story even in a release build
/// (whose `error` base would otherwise drop every diagnostic) — the
/// same rationale as the `messages=info` pin. tracing writes only to
/// the file sink; `--output json` (stdout) is a separate path, so this
/// never affects the event stream.
#[must_use]
pub fn log_filter() -> tracing_subscriber::EnvFilter {
    use tracing_subscriber::EnvFilter;
    const SUBSYSTEMS: &str = "agent_square::gossip=info,\
        agent_square::lookup=info,\
        agent_square::beacon=info,\
        agent_square::lifecycle=info,\
        agent_square::directory=info,\
        agent_square::messages=info";
    EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(if cfg!(debug_assertions) {
            format!("info,noq_proto::connection=off,{SUBSYSTEMS}")
        } else {
            format!("error,noq_proto::connection=off,mainline::rpc=off,{SUBSYSTEMS}")
        })
    })
}
