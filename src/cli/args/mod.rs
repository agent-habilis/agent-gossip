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
mod exchange;
mod join;
mod lookup;
mod meta;
mod msg;
mod output;
mod peers;
mod ping;
mod pipe;
mod poll;
mod ready;
mod shared;
mod state;

pub(crate) use create::CreateOpts;
pub(crate) use discover::DiscoverOpts;
pub(crate) use exchange::ExchangeOpts;
pub(crate) use join::JoinOpts;
pub(crate) use meta::{MetaAction, MetaOpts};
pub(crate) use msg::MsgOpts;
pub(crate) use output::OutputFormat;
pub(crate) use peers::PeersOpts;
pub(crate) use ping::PingOpts;
pub(crate) use pipe::PipeAction;
pub(crate) use poll::PollOpts;
pub(crate) use ready::ReadyOpts;
pub(crate) use shared::SharedServerOpts;
pub(crate) use state::{StateAction, StateOpts};

#[derive(Parser, Debug)]
#[command(
    name = "ahsw",
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

    /// Send one leg of an exchange to a specific peer.
    ///
    /// An exchange is a directed, phased conversation correlated by `--exchange-id`
    /// (offer → accept → context → done → confirm/change). `handover` is
    /// one behavior built on it. The receiving daemon surfaces an `exchange` (or
    /// `exchange_progress`) event on its `--output json` stream. `--phase offer`
    /// validates `--to` against the live roster and errors on an unknown
    /// nickname.
    Exchange {
        #[command(flatten)]
        opts: ExchangeOpts,
    },

    /// List the live participant roster of a swarm.
    ///
    /// Queries the running daemon for current participants (nicknames +
    /// how long ago each was last seen), recency-sorted. Backs the
    /// handover target picker; prints a JSON object with `participants`
    /// and `participant_count`.
    Peers {
        #[command(flatten)]
        opts: PeersOpts,
    },

    /// Stream stdin to a peer, or a peer's stream to stdout (off-gossip, direct P2P).
    ///
    /// A standalone byte pipe: `pipe listen` reads stdin and prints the
    /// `ahsw pipe connect 🐝…` command on stdout; `pipe connect <ticket>` streams
    /// the bytes to stdout. No running daemon required.
    Pipe {
        #[command(subcommand)]
        action: PipeAction,
    },

    /// Read or change the swarm's shared state.
    ///
    /// Shared state is a JSON document every member derives from a dedicated,
    /// gossiped log of JSON-Patch changes. `state patch` applies an RFC 6902
    /// patch; `state get` prints the current document. Members react to a
    /// change via the `state` event on the daemon's `--output json` stream.
    State {
        #[command(flatten)]
        opts: StateOpts,
    },

    /// Read or change the swarm's `meta` channel.
    ///
    /// A second shared-state document beside `state`, identical machinery but
    /// conventionally holding swarm metadata (peer info, …) rather than the task.
    /// `meta patch` applies an RFC 6902 patch; `meta get` prints the current
    /// document. The two channels are fully independent.
    Meta {
        #[command(flatten)]
        opts: MetaOpts,
    },

    /// Wait until a swarm daemon is serving, then exit.
    ///
    /// The readiness gate for driving the daemon over the CLI: launch
    /// `create`/`join` in the background with a `--state-file`, then
    /// `ahsw ready --state-file <path>` blocks until that file reports the
    /// daemon is serving, exiting 0 (non-zero on timeout). In `human` mode a
    /// silent gate (exit code only); with `--output json` it prints
    /// `{swarm,name,nickname}` on success, so the gate doubles as the
    /// identity read.
    Ready {
        #[command(flatten)]
        opts: ReadyOpts,
    },

    /// Run as a Model Context Protocol server over stdio.
    ///
    /// Exposes swarm lifecycle + messaging as MCP tools for AI clients
    /// (Codex, Cursor, Claude Desktop, Claude Code). Reads JSON-RPC from
    /// stdin, writes to stdout; the caller is expected to be an MCP client
    /// that manages this process's lifetime.
    Mcp {
        /// Browse directories over the loopback ladder and relax the
        /// advertise→public guard (test-only). Mirrors the server commands'
        /// hidden flag so the directory path runs hermetically.
        #[arg(long, hide = true, default_value_t = false)]
        directory_private: bool,

        /// How long `ping` collects pongs (seconds). Hidden; tests shorten it
        /// so a `ping` round-trip doesn't wait the full window.
        #[arg(long, hide = true, default_value_t = crate::util::consts::PING_WINDOW_SECS)]
        ping_window_secs: u64,
    },

    /// Print the full agent manual to stdout.
    ///
    /// A self-contained man page covering every command, JSON event, and
    /// common workflow, embedded in the binary so it works with no repo
    /// checkout.
    Man,

    /// Plug the swarm integrations into your agents.
    ///
    /// Targets Claude Code (the plugin), pi (the extension), and a generic
    /// `~/.agents/skills` agent. The artifacts are embedded in the binary, so
    /// this needs no repo checkout. With no `--agent`, the detected agents are
    /// used. Reversible with `unplug`.
    Plug {
        /// Agent(s) to install into (repeatable). Defaults to detected agents.
        #[arg(long = "agent", value_enum)]
        agents: Vec<super::agent::Agent>,
    },

    /// Unplug the swarm integrations from your agents (symmetric to `plug`).
    ///
    /// With no `--agent`, the agents that have it are used.
    Unplug {
        /// Agent(s) to remove from (repeatable). Defaults to agents that have
        /// the integration installed.
        #[arg(long = "agent", value_enum)]
        agents: Vec<super::agent::Agent>,
    },

    /// Report which agents have the swarm integrations installed.
    ///
    /// Read-only: for each agent (Claude Code, pi, generic) prints whether the
    /// integration is set up, not set up, or the agent is absent, plus its path.
    Status,
}
