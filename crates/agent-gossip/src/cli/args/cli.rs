use clap::Parser;

use super::commands::Commands;

#[derive(Parser, Debug)]
#[command(
    name = "agent-gossip",
    about = "mesh network for agents",
    version = crate::version(),
    after_help = "a tool by agent-habilis █🫈"
)]
pub(crate) struct Cli {
    /// Per-member log directory (default: the per-user runtime base, see
    /// `habilis_network::util::runtime_base`, with a per-mesh `<prefix>/` subfolder).
    /// Hidden — a test/ops knob.
    /// Global so it applies to any subcommand.
    #[arg(long, global = true, hide = true)]
    pub log_dir: Option<std::path::PathBuf>,

    /// Max log-file bytes before rotating to `<file>.1` (`0` disables).
    /// Hidden test/ops knob; default `habilis_network::util::tuning::LOG_FILE_MAX_BYTES`.
    #[arg(long, global = true, hide = true)]
    pub log_max_bytes: Option<u64>,

    /// Log raw message bodies (default: redacted to length + content-hash so
    /// log files are safe to share). Hidden opt-in for a dev's own local
    /// debugging — never set on a user's machine if the logs may be sent
    /// upstream.
    #[arg(long, global = true, hide = true)]
    pub log_raw: bool,

    #[command(subcommand)]
    pub command: Commands,
}
