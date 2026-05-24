//! Developer-log plumbing: the tracing directive filter ([`log_filter`])
//! and the deferred per-member file sink ([`sink`]). `--output json`
//! (stdout) is a separate path and is unaffected by anything here.

mod sink;

pub use sink::LogSink;
pub(crate) use sink::{attach, detach, flush_pending_to_stderr, install};

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
pub(crate) fn log_filter() -> tracing_subscriber::EnvFilter {
    use tracing_subscriber::EnvFilter;
    const SUBSYSTEMS: &str = "agent_habilis_swarm::gossip=info,\
        agent_habilis_swarm::lookup=info,\
        agent_habilis_swarm::beacon=info,\
        agent_habilis_swarm::lifecycle=info,\
        agent_habilis_swarm::directory=info,\
        agent_habilis_swarm::messages=info";
    EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(if cfg!(debug_assertions) {
            format!("info,noq_proto::connection=off,{SUBSYSTEMS}")
        } else {
            format!("error,noq_proto::connection=off,mainline::rpc=off,{SUBSYSTEMS}")
        })
    })
}
