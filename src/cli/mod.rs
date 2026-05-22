use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use iroh::RelayUrl;
use serde::Deserialize;

use crate::daemon::run as run_event_loop;
use crate::daemon::setup::{SetupKind, setup_swarm};
use crate::output::{Output, OutputMode};
use crate::protocol::swarm::{DiscoveryOpts, LookupSet, Swarm, SwarmMode, SwarmName, resolve_discovery};
use crate::protocol::{MessageBody, MessageId, Nickname, SwarmId};
use crate::resolver;
use crate::transport::ipc::{self, IpcCommand};
use crate::util::tuning::DEFAULT_MAX_DIRECT_PEERS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum OutputFormat {
    Human,
    Json,
}

#[derive(Parser, Debug)]
#[command(
    name = "ahs",
    about = "swarm network for agents",
    version,
    after_help = "a tool by 🫈 agent-habilis"
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

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

    /// Max direct peer connections (gossip relays beyond this)
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

    /// Custom relay URL (connectivity). Omitted ⇒ iroh's N0 default
    /// relays on `public` (the relay is never disabled — it is a URL,
    /// not a toggle). Requires `--public`; per-process, like
    /// the lookup flags.
    #[arg(long)]
    pub relay: Option<RelayUrl>,

    /// Which address-lookups to enable.
    #[command(flatten)]
    pub lookups: LookupArgs,
}

/// The address-lookup selection flags (presence allowlist): with
/// `--public`, naming none enables all (mdns+dht); naming any
/// uses *only* those passed. Both require `--public`. Grouped
/// and flattened so each options struct stays within the readable
/// bool budget.
#[derive(Parser, Debug)]
pub(crate) struct LookupArgs {
    /// Enable the LAN mDNS address-lookup.
    #[arg(long, default_value_t = false)]
    pub mdns: bool,

    /// Enable the mainline-DHT address-lookup.
    #[arg(long, default_value_t = false)]
    pub dht: bool,
}

impl LookupArgs {
    fn to_set(&self) -> LookupSet {
        LookupSet {
            mdns: self.mdns,
            dht: self.dht,
        }
    }
}

#[derive(Parser, Debug)]
pub(crate) struct CreateOpts {
    #[command(flatten)]
    pub shared: SharedServerOpts,

    /// Human-readable swarm name. Optional — a random word-word name is
    /// minted if omitted. Same rules as a nickname: 1..=32 UTF-8
    /// characters (any script/emoji), excluding control characters,
    /// whitespace, and path separators (/ \). Bound cryptographically
    /// into the swarm identity so joiners who decode the ID see the same
    /// name and a forged ID with a fake name fails to find peers.
    #[arg(long)]
    pub name: Option<SwarmName>,

    /// Use the public network (cross-machine via iroh DNS + N0 or
    /// custom relay). Omitted ⇒ private (loopback only, the
    /// default). Relay/lookup selection lives in the shared
    /// `--relay`/`--mdns`/`--dht` flags.
    #[arg(long, default_value_t = false)]
    pub public: bool,

    /// Optional nickname (random word-word if not provided). A custom
    /// nickname is 1..=32 UTF-8 characters, excluding control chars,
    /// whitespace, and path separators. Symmetric with `ahs join
    /// --nickname`.
    #[arg(long)]
    pub nickname: Option<Nickname>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_opts_with_nickname() {
        let cli = Cli::parse_from(["ahs", "create", "--name", "team", "--nickname", "my-nick"]);
        match cli.command {
            Commands::Create { opts } => {
                assert_eq!(opts.name.as_ref().map(SwarmName::as_str), Some("team"));
                assert_eq!(
                    opts.nickname.as_ref().map(Nickname::as_str),
                    Some("my-nick")
                );
            }
            _ => panic!("expected Create command"),
        }
    }

    #[test]
    fn create_opts_without_nickname() {
        let cli = Cli::parse_from(["ahs", "create", "--name", "team"]);
        match cli.command {
            Commands::Create { opts } => {
                assert_eq!(opts.name.as_ref().map(SwarmName::as_str), Some("team"));
                assert_eq!(opts.nickname, None);
            }
            _ => panic!("expected Create command"),
        }
    }

    #[test]
    fn create_opts_name_optional() {
        let cli = Cli::parse_from(["ahs", "create"]);
        match cli.command {
            Commands::Create { opts } => assert_eq!(opts.name, None),
            _ => panic!("expected Create command"),
        }
    }

    #[test]
    fn create_opts_rejects_invalid_name() {
        assert!(Cli::try_parse_from(["ahs", "create", "--name", ""]).is_err());
        assert!(
            Cli::try_parse_from(["ahs", "create", "--name", "has space"]).is_err(),
            "whitespace must reject"
        );
        assert!(
            Cli::try_parse_from(["ahs", "create", "--name", "a/b"]).is_err(),
            "path separator must reject"
        );
        assert!(
            Cli::try_parse_from(["ahs", "create", "--name", &"a".repeat(33)]).is_err(),
            "33 chars must reject"
        );
    }

    #[test]
    fn create_opts_nickname_with_other_flags() {
        let cli = Cli::parse_from([
            "ahs",
            "create",
            "--name",
            "team",
            "--public",
            "--nickname",
            "custom-name",
            "--no-interactive",
        ]);
        match cli.command {
            Commands::Create { opts } => {
                assert_eq!(opts.name.as_ref().map(SwarmName::as_str), Some("team"));
                assert_eq!(
                    opts.nickname.as_ref().map(Nickname::as_str),
                    Some("custom-name")
                );
                assert!(opts.public);
                assert!(opts.shared.no_interactive);
            }
            _ => panic!("expected Create command"),
        }
    }
}

#[derive(Parser, Debug)]
pub(crate) struct JoinOpts {
    #[command(flatten)]
    pub shared: SharedServerOpts,
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
        /// Swarm identifier (ahs...), a domain (example.com), or a git repo
        /// URL (github.com/user/repo, gitlab.com/user/repo, bitbucket.org/user/repo).
        /// Non-id values are resolved via /.well-known/agent-habilis-swarm.
        swarm: String,

        /// Optional nickname (random word-word if not provided). A custom
        /// nickname is 1..=32 UTF-8 characters, excluding control chars,
        /// whitespace, and path separators.
        #[arg(long)]
        nickname: Option<Nickname>,

        /// Accepted only to emit a clear error: the network mode is
        /// encoded in the swarm id, so `join` has no `--public`.
        #[arg(long, hide = true)]
        public: bool,

        /// Accepted only to emit a clear error: the swarm name is
        /// encoded in the swarm id, so `join` has no `--name`.
        #[arg(long, hide = true)]
        name: Option<String>,

        #[command(flatten)]
        opts: JoinOpts,
    },

    /// Post a message to a swarm
    Msg {
        /// Swarm identifier (ahs...)
        #[arg(long)]
        swarm: SwarmId,

        /// Nickname of the local agent to post as (must have a running join/create session)
        #[arg(long)]
        nickname: Nickname,

        /// The message text (UTF-8; newlines/tabs allowed, other control
        /// characters rejected)
        #[arg(long)]
        text: MessageBody,

        /// Address this message to a specific peer's nickname
        #[arg(long)]
        reply: Option<Nickname>,
    },

    /// Check for new messages in a swarm
    Poll {
        /// Swarm identifier (ahs...)
        #[arg(long)]
        swarm: SwarmId,

        /// Nickname of the local agent (must have a running join/create session)
        #[arg(long)]
        nickname: Nickname,

        /// Only return messages after this message ID
        #[arg(long)]
        after: Option<MessageId>,

        /// Output format: human (default) or json (structured JSON)
        #[arg(long, default_value = "human")]
        output: OutputFormat,
    },

    /// Run as a Model Context Protocol server over stdio.
    ///
    /// Exposes swarm lifecycle + messaging as MCP tools for AI clients
    /// (Codex, Cursor, Claude Desktop, Claude Code). Reads JSON-RPC from
    /// stdin, writes to stdout; the caller is expected to be an MCP client
    /// that manages this process's lifetime.
    Mcp,
}

// ── dispatch ─────────────────────────────────────────────────────
//
// The per-subcommand bodies live here (not in a separate `commands`
// module) so the CLI is one place: clap shape + what each command
// does. `lib.rs::run_cli` is a thin shim that parses argv and calls
// `dispatch`.

impl From<OutputFormat> for OutputMode {
    fn from(fmt: OutputFormat) -> Self {
        match fmt {
            OutputFormat::Human => OutputMode::Human,
            OutputFormat::Json => OutputMode::Json,
        }
    }
}

/// `join` has no `--public`/`--name`: both are encoded in the `ahs…`
/// identifier and auto-detected. Without this, clap rejects them with
/// a generic "unexpected argument" + a misleading "pass as a value"
/// tip; this gives the real reason instead.
fn reject_id_encoded_flag(flag: &str, present: bool) -> Result<()> {
    if present {
        anyhow::bail!(
            "`{flag}` is not valid for `join`: the swarm's network mode \
             and name are encoded in the swarm id and auto-detected. \
             Drop `{flag}` — `join` takes only the id and `--nickname`."
        );
    }
    Ok(())
}

/// Run the selected subcommand to completion.
///
/// # Errors
/// Propagates any error from the selected subcommand — swarm setup
/// failure, join timeout, IPC errors, invalid swarm-mode flags, etc.
pub(crate) async fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Create { opts } => create(opts).await,
        Commands::Join {
            swarm,
            nickname,
            public,
            name,
            opts,
        } => {
            reject_id_encoded_flag("--public", public)?;
            reject_id_encoded_flag("--name", name.is_some())?;
            join(&swarm, nickname, opts).await
        }
        Commands::Msg {
            swarm,
            nickname,
            text,
            reply,
        } => msg(&swarm, &nickname, text, reply).await,
        Commands::Poll {
            swarm,
            nickname,
            after,
            output: _,
        } => poll(&swarm, &nickname, after).await,
        Commands::Mcp => crate::mcp::run().await,
    }
}

/// Build the output sink, resolve the author, set up the swarm, and
/// run the event loop. The shared spine of `create` and `join`.
async fn run_session(
    kind: SetupKind,
    discovery: DiscoveryOpts,
    shared: SharedServerOpts,
    nickname: Option<Nickname>,
) -> Result<()> {
    let out = Output::new(shared.output.into(), shared.filter_self);
    let author = nickname.unwrap_or_else(Nickname::random);
    let cfg = setup_swarm(
        kind,
        author,
        !shared.no_interactive,
        shared.max_peers,
        shared.state_file,
        discovery,
        out,
    )
    .await?;
    // First point where swarm id + nickname are known — attach the
    // buffered log sink here (see `logsink`).
    crate::logsink::attach(&cfg.swarm, &cfg.author);
    run_event_loop(cfg).await
}

/// Create a new swarm, print its identifier, and start listening.
async fn create(opts: CreateOpts) -> Result<()> {
    let mode = if opts.public {
        SwarmMode::Public
    } else {
        SwarmMode::Private
    };
    let discovery = resolve_discovery(
        mode,
        opts.shared.lookups.to_set(),
        opts.shared.relay.clone(),
    )?;
    let name = opts.name.unwrap_or_else(SwarmName::random);
    let kind = SetupKind::Create { mode, name };
    run_session(kind, discovery, opts.shared, opts.nickname).await
}

/// Join an existing swarm by its identifier (ahs...), a domain, or a
/// supported git repo URL. The network mode is decoded from the id;
/// discovery flags are per-process (the joiner opts in itself).
async fn join(swarm_input: &str, nickname: Option<Nickname>, opts: JoinOpts) -> Result<()> {
    let swarm: Swarm = resolver::resolve(swarm_input).await?;
    let discovery = resolve_discovery(
        swarm.mode,
        opts.shared.lookups.to_set(),
        opts.shared.relay.clone(),
    )?;
    run_session(SetupKind::Join { swarm }, discovery, opts.shared, nickname).await
}

#[derive(Deserialize)]
struct MsgResponse {
    ok: bool,
    id: Option<String>,
    error: Option<String>,
}

/// Post a message to a swarm via the running server's IPC socket.
async fn msg(
    swarm: &SwarmId,
    nickname: &Nickname,
    text: MessageBody,
    reply: Option<Nickname>,
) -> Result<()> {
    let cmd = IpcCommand::Msg {
        swarm: swarm.clone(),
        body: text,
        reply,
    };

    let resp = ipc::send(&cmd, nickname).await?;
    let parsed: MsgResponse = serde_json::from_str(&resp)?;

    if !parsed.ok {
        anyhow::bail!(
            "{}",
            parsed.error.unwrap_or_else(|| "unknown error".to_string())
        );
    }

    let id = parsed.id.unwrap_or_default();
    // `msg` has no `--output` flag — always the human confirmation.
    let out = Output::new(OutputMode::Human, false);
    out.msg_posted(&id);

    Ok(())
}

/// Retrieve buffered messages from a running swarm process via IPC.
async fn poll(swarm: &SwarmId, nickname: &Nickname, after: Option<MessageId>) -> Result<()> {
    let cmd = IpcCommand::Poll {
        swarm: swarm.clone(),
        after,
    };

    let resp = ipc::send(&cmd, nickname).await?;
    println!("{resp}");

    Ok(())
}
