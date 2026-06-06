//! The command-line surface: `Cli` + the `Commands` enum tie the
//! per-command argument structs together. Each command's args (and its
//! parse tests) live in their own file — `create`, `join`, `msg`,
//! `poll`, `ping`, `discover` — over the shared building blocks
//! `shared` (`SharedServerOpts`), `lookup` (`LookupArgs`), and `output`
//! (`OutputFormat`). The imperative per-command logic lives in the
//! parent [`super`] module.

use clap::{Parser, Subcommand};

mod create;
mod discover;
mod join;
mod lookup;
mod msg;
mod output;
mod ping;
mod poll;
mod shared;

pub(crate) use create::CreateOpts;
pub(crate) use discover::DiscoverOpts;
pub(crate) use join::JoinOpts;
pub(crate) use msg::MsgOpts;
pub(crate) use output::OutputFormat;
pub(crate) use ping::PingOpts;
pub(crate) use poll::PollOpts;
pub(crate) use shared::SharedServerOpts;

#[derive(Parser, Debug)]
#[command(
    name = "ahs",
    about = "swarm network for agents",
    version = crate::util::version::VERSION,
    after_help = "a tool by agent-habilis █🫈"
)]
pub(crate) struct Cli {
    /// Per-member log directory (default: the OS temp dir). Hidden — a
    /// test/ops knob; production reads `crate::util::consts::LOG_SUBPATH`
    /// under the temp dir. Global so it applies to any subcommand.
    #[arg(long, global = true, hide = true)]
    pub log_dir: Option<std::path::PathBuf>,

    /// Max log-file bytes before rotating to `<file>.1` (`0` disables).
    /// Hidden test/ops knob; default `crate::util::consts::LOG_FILE_MAX_BYTES`.
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

#[derive(Subcommand, Debug)]
pub(crate) enum Commands {
    /// Create and join a new swarm
    Create {
        #[command(flatten)]
        opts: CreateOpts,
    },

    /// Join an existing swarm
    Join {
        #[command(flatten)]
        opts: JoinOpts,
    },

    /// Post a message to a swarm
    Msg {
        #[command(flatten)]
        opts: MsgOpts,
    },

    /// Check for new messages in a swarm
    Poll {
        #[command(flatten)]
        opts: PollOpts,
    },

    /// Ping all peers and have the daemon measure RTT. Fire-and-forget:
    /// the `ping_report` arrives on the running create/join daemon's
    /// `--output json` stream, not on this command's stdout.
    Ping {
        #[command(flatten)]
        opts: PingOpts,
    },

    /// Browse swarms advertising themselves in a directory.
    ///
    /// Joins the directory and shows a live list of swarms
    /// created with `--advertise`. Interactive (default): pick a number
    /// to join. `--no-interactive` / `--output json`: stream
    /// `swarm_found` / `swarm_lost` JSON lines for an agent to act on.
    Discover {
        #[command(flatten)]
        opts: DiscoverOpts,
    },

    /// Run as a Model Context Protocol server over stdio.
    ///
    /// Exposes swarm lifecycle + messaging as MCP tools for AI clients
    /// (Codex, Cursor, Claude Desktop, Claude Code). Reads JSON-RPC from
    /// stdin, writes to stdout; the caller is expected to be an MCP client
    /// that manages this process's lifetime.
    Mcp,

    /// Print the full agent manual to stdout.
    ///
    /// A self-contained man page covering every command, JSON event, and
    /// common workflow, embedded in the binary so it works with no repo
    /// checkout.
    Man,
}
