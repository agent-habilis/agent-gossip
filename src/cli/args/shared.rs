//! `SharedServerOpts` — the option group flattened into every
//! long-running server command (`create`, `join`, `discover`).

use clap::Parser;

use crate::util::tuning::DEFAULT_MAX_DIRECT_PEERS;

use super::lookup::LookupArgs;
use super::output::OutputFormat;

/// Shared options for server commands.
#[derive(Parser, Debug)]
pub(crate) struct SharedServerOpts {
    /// Disable interactive message input from stdin
    #[arg(long, default_value_t = false)]
    pub no_interactive: bool,

    /// Output format: human (default) or json (structured JSON lines)
    #[arg(long, default_value = "human")]
    pub output: OutputFormat,

    /// Suppress self-authored messages from stdout (for Monitor use)
    #[arg(long, default_value_t = false)]
    pub filter_self: bool,

    /// Soft ceiling on tracked peer addresses (gossip relays beyond
    /// this). Note: the gossip overlay maintains HyParView's
    /// `active_view_capacity` (5) active neighbors regardless — this is
    /// not the live connection count.
    #[arg(long, default_value_t = DEFAULT_MAX_DIRECT_PEERS)]
    pub max_peers: usize,

    /// Session state file. When set, the daemon merges
    /// `{swarm, nickname, participant_count, last_updated}` into this
    /// JSON file — preserving any other keys, e.g. those written by
    /// the `/swarm:*` skills — on every peer set change and on a
    /// ~10s heartbeat, and deletes the file on clean shutdown. Used
    /// by external tools (e.g. a shell statusline) to render live
    /// participant count and liveness without IPC.
    #[arg(long)]
    pub state_file: Option<std::path::PathBuf>,

    /// Which lookup mechanisms to enable.
    #[command(flatten)]
    pub lookups: LookupArgs,
}
