//! The command-line surface: `Cli` + the `Commands` enum tie the
//! per-command argument structs together. Each command's args (and its
//! parse tests) live in their own file — `create`, `join`, `msg`,
//! `poll`, `ping`, `discover` — over the shared building blocks
//! `shared` (`SharedServerOpts`) and `lookup` (`LookupArgs`). The
//! imperative per-command logic lives in the parent [`super`] module.

use clap::{Parser, Subcommand};

mod a2a;
mod create;
mod discover;
mod doctor;
mod invite;
mod join;
mod leave;
mod legacy;
mod lookup;
mod meta;
mod peers;
mod ping;
mod poll;
mod ready;
mod session;
mod shared;
mod state;
mod topic;
mod topology;
mod tuning;

pub(crate) use a2a::{A2aAction, A2aOpts};
pub(crate) use create::CreateOpts;
pub(crate) use discover::DiscoverOpts;
pub(crate) use doctor::{DoctorOpts, OutputFormat};
pub(crate) use invite::InviteOpts;
pub(crate) use join::JoinOpts;
pub(crate) use leave::LeaveOpts;
pub(crate) use meta::{MetaAction, MetaOpts};
pub(crate) use peers::PeersOpts;
pub(crate) use ping::PingOpts;
pub(crate) use poll::PollOpts;
pub(crate) use ready::ReadyOpts;
pub(crate) use session::SessionOpts;
pub(crate) use shared::SharedServerOpts;
pub(crate) use state::{StateAction, StateOpts};
pub(crate) use topic::TopicOpts;
pub(crate) use topology::TopologyOpts;

/// `--version` string: the **app** crate's version plus the engine's git
/// stamp. The engine's ready-made `VERSION` leads with the engine crate's
/// number instead, which read as a stale release during the fossil-stamp
/// incident (engine 0.5.0 vs app 0.6.0). Assembled at runtime because
/// `concat!` cannot splice a cross-crate const; parked in a `OnceLock` for
/// the `&'static str` clap's `version` requires.
fn cli_version() -> &'static str {
    static CLI_VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CLI_VERSION.get_or_init(|| {
        format!(
            "{} {}",
            env!("CARGO_PKG_VERSION"),
            agent_habilis_mesh::util::version::GIT_STAMP
        )
    })
}

#[derive(Parser, Debug)]
#[command(
    name = "agent-square",
    about = "mesh network for agents",
    version = cli_version(),
    after_help = "a tool by agent-habilis █🫈"
)]
pub(crate) struct Cli {
    /// Per-member log directory (default: the per-user runtime base, see
    /// `agent_habilis_mesh::util::runtime_base`, with a per-mesh `<prefix>/` subfolder).
    /// Hidden — a test/ops knob.
    /// Global so it applies to any subcommand.
    #[arg(long, global = true, hide = true)]
    pub log_dir: Option<std::path::PathBuf>,

    /// Max log-file bytes before rotating to `<file>.1` (`0` disables).
    /// Hidden test/ops knob; default `agent_habilis_mesh::util::consts::LOG_FILE_MAX_BYTES`.
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
    /// Create and join a new square
    Create {
        #[command(flatten)]
        opts: CreateOpts,
    },

    /// Join an existing square
    Join {
        #[command(flatten)]
        opts: JoinOpts,
    },

    /// Join a public square derived from a shared string
    Topic {
        #[command(flatten)]
        opts: TopicOpts,
    },

    /// Leave a square: stop this session's local daemon.
    ///
    /// Finds running create/join/topic daemons through their state files
    /// and sends each a SIGTERM; a daemon broadcasts `left` to its peers
    /// and removes its state file on the way out. With no SQUARE, stops
    /// only the daemons owned by the calling session — those with
    /// `--session-pid` among their process ancestors — and never touches
    /// other sessions'. An explicit SQUARE targets that square's local
    /// member(s) regardless of owner. State files whose daemon is gone
    /// (SIGKILL, power loss) are cleaned up along the way.
    Leave {
        #[command(flatten)]
        opts: LeaveOpts,
    },

    /// Report the square this session is joined to.
    ///
    /// The read-only sibling of `leave`: discovers running daemons through
    /// their state files and prints the ones owned by the calling session
    /// (see `--session-pid`). This is how an agent that lost its
    /// conversation context (e.g. after a context clear) re-learns the
    /// square id and nickname of a session it is still joined to.
    Session {
        #[command(flatten)]
        opts: SessionOpts,
    },

    /// Check for new messages in a square
    Poll {
        #[command(flatten)]
        opts: PollOpts,
    },

    /// Ping all peers and have the daemon measure RTT.
    Ping {
        #[command(flatten)]
        opts: PingOpts,
    },

    /// Browse squares advertising themselves in a directory.
    ///
    /// Joins the directory and streams `square_found` / `square_lost` JSON
    /// lines for squares created with `--advertise` — capture an id and call
    /// `join` yourself. Does not auto-join.
    Discover {
        #[command(flatten)]
        opts: DiscoverOpts,
    },

    /// Bridge an A2A (agent-to-agent) HTTP server to a peer over the square.
    ///
    /// `a2a expose --to http://127.0.0.1:PORT` runs next to a local A2A server
    /// and prints a `🎟️…` ticket; `a2a connect <ticket>` binds a local endpoint
    /// an unmodified A2A client points at, tunnelling its requests to the
    /// exposer. The Agent Card's URLs are rewritten so discovery resolves
    /// through the bridge. Strictly 1:1 — one consumer per exposer at a time.
    A2a {
        #[command(flatten)]
        opts: A2aOpts,
    },

    /// List the live participant roster of a square.
    ///
    /// Queries the running daemon for current participants (nicknames +
    /// how long ago each was last seen), recency-sorted. Backs the
    /// task target picker; prints a JSON object with `participants`
    /// and `participant_count`.
    Peers {
        #[command(flatten)]
        opts: PeersOpts,
    },

    /// Mint a ticket to an invite-only square
    ///
    /// The creating session's daemon signs the invite with its in-memory issuer
    /// key and prints the `🎟️…` token — share it so a peer can `join` with it.
    /// After the creator's daemon restarts the issuer key is gone, so no new
    /// invites can be minted (already-issued ones still redeem). Password: an
    /// invite inherits the square's password, so a scraped invite still needs it.
    Invite {
        #[command(flatten)]
        opts: InviteOpts,
    },

    /// Read or change the square's shared state.
    ///
    /// Shared state is a JSON document every member derives from a dedicated,
    /// gossiped log of RFC 7386 JSON Merge Patch changes. `state merge` applies a
    /// merge; `state get` prints the current document. Members react to a
    /// change via the `state` event on the daemon's `--output json` stream.
    State {
        #[command(flatten)]
        opts: StateOpts,
    },

    /// Read or change the square's meta state.
    ///
    /// A second shared-state document beside `state`, identical machinery but
    /// conventionally holding square metadata (peer info, …) rather than the task.
    /// `meta merge` applies an RFC 7386 JSON Merge Patch; `meta get` prints the
    /// current document. The two channels are fully independent.
    Meta {
        #[command(flatten)]
        opts: MetaOpts,
    },

    /// Print the circuit routing topology from a running daemon's point of view.
    ///
    /// Emits the metric-weighted square graph the daemon has assembled from
    /// gossiped link-state, as JSON (`{self_id, edges:[{from,to,metric}]}`) —
    /// the data behind the `/square:topology` render.
    Topology {
        #[command(flatten)]
        opts: TopologyOpts,
    },

    /// Wait until a square daemon is serving, then exit.
    ///
    /// The readiness gate for driving the daemon over the CLI: launch
    /// `create`/`join` in the background with a `--state-file`, then
    /// `agent-square ready --state-file <path>` blocks until that file reports the
    /// daemon is serving, exiting 0 (non-zero on timeout). In `human` mode a
    /// silent gate (exit code only); with `--output json` it prints
    /// `{square,name,nickname}` on success, so the gate doubles as the
    /// identity read.
    Ready {
        #[command(flatten)]
        opts: ReadyOpts,
    },

    /// Run as a Model Context Protocol server over stdio.
    ///
    /// Exposes square lifecycle + messaging as MCP tools for AI clients
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
        #[arg(long, hide = true, default_value_t = agent_habilis_mesh::util::consts::PING_WINDOW_SECS)]
        ping_window_secs: u64,

        /// How long a `long: true` fetch parks before returning empty (millis).
        /// Hidden; tests shorten it to hit the timeout path quickly.
        #[arg(long, hide = true, default_value_t = agent_habilis_mesh::util::consts::LONGPOLL_MAX_MS)]
        longpoll_max_ms: u64,
    },

    /// Print the full agent manual to stdout.
    ///
    /// A self-contained man page covering every command, JSON event, and
    /// common workflow, embedded in the binary so it works with no repo
    /// checkout.
    Man,

    /// Plug the square integrations into your agents.
    ///
    /// Targets Claude Code, pi, Codex, and Cursor. The skills are embedded in
    /// the binary, so this needs no repo checkout. With no `--agent`, the
    /// detected agents are used; an agent that is not on this machine is
    /// skipped, never scaffolded. Ends by listing every supported agent and
    /// whether it was installed. Reversible with `unplug`.
    Plug {
        /// Agent to install into (repeatable). Defaults to detected agents.
        #[arg(long = "agent", value_enum)]
        agents: Vec<super::agent::Agent>,
        /// Install into an explicit directory as a skill root (repeatable). No
        /// agent detection — the skills are written there unconditionally.
        #[arg(long = "path")]
        paths: Vec<std::path::PathBuf>,
    },

    /// Remove the square integrations from your agents.
    ///
    /// With no `--agent`, the agents that have it are used.
    Unplug {
        /// Agent to remove from (repeatable). Defaults to agents that have
        /// the integration installed.
        #[arg(long = "agent", value_enum)]
        agents: Vec<super::agent::Agent>,
        /// Remove from an explicit directory that was plugged with `--path`
        /// (repeatable). Only the installed skill dirs are removed; the folder
        /// and anything else in it are left.
        #[arg(long = "path")]
        paths: Vec<std::path::PathBuf>,
    },

    /// Diagnose the square environment and network.
    ///
    /// With no `--square`: a machine-health report — binary/OS, which agents
    /// have the integration installed (and where), the local network
    /// capability (UDP, NAT/hole-punch behavior, public address, relay
    /// latency), and the squares running on this machine. With `--square <💬…>`:
    /// decode that square's declared connection methods and live-probe which
    /// reach it (down to direct-vs-relay path per peer). `--output json` for
    /// the machine form.
    Doctor {
        #[command(flatten)]
        opts: DoctorOpts,
    },
}
