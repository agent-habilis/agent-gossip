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
    after_help = "a tool by 🫈 agent-habilis"
)]
pub(crate) struct Cli {
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
}
