use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use iroh::RelayUrl;
use serde::Deserialize;

use crate::daemon::run as run_event_loop;
use crate::daemon::setup::{SetupKind, setup_swarm};
use crate::embed::{Directory, DirectoryEvent, SwarmListing, spawn_advertiser};
use crate::output::{Output, OutputMode};
use crate::protocol::swarm::{
    DirectorySelection, LookupOpts, LookupSet, RelaySelection, Swarm, SwarmMode, SwarmName,
    resolve_lookups, validate_advertise,
};
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

/// The lookup allowlist flags: with `--public`, naming none enables
/// all three (mdns + dht + pinned relay); naming any uses *only* those
/// passed (so `--mdns` alone disables both dht and the relay). All
/// require `--public`. Grouped and flattened so each options struct
/// stays within the readable bool budget.
#[derive(Parser, Debug)]
pub(crate) struct LookupArgs {
    /// Enable the LAN mDNS address-lookup.
    #[arg(long, default_value_t = false)]
    pub mdns: bool,

    /// Enable the mainline-DHT address-lookup.
    #[arg(long, default_value_t = false)]
    pub dht: bool,

    /// Enable the relay (connectivity + relay-direct rendezvous). Bare
    /// `--relay` ⇒ the pinned default relay; `--relay <URL>` ⇒ a custom
    /// relay. Omitting it while naming another flag disables the relay;
    /// naming no flag at all enables it at the pinned default. An
    /// allowlist member like `--mdns`/`--dht` — per-process, requires
    /// `--public`. Absent ⇒ `None`; bare ⇒ `Some(None)`; valued ⇒
    /// `Some(Some(url))`.
    #[arg(long, num_args(0..=1))]
    #[expect(
        clippy::option_option,
        reason = "clap optional-value flag: absent/bare/valued are three distinct relay states (see RelaySelection)"
    )]
    pub relay: Option<Option<RelayUrl>>,
}

impl LookupArgs {
    fn to_set(&self) -> LookupSet {
        let relay = match &self.relay {
            None => RelaySelection::Unset,
            Some(None) => RelaySelection::Default,
            Some(Some(url)) => RelaySelection::Custom(url.clone()),
        };
        LookupSet {
            mdns: self.mdns,
            dht: self.dht,
            relay,
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
    /// whitespace, and any of / \ < > # (the last three are reserved for
    /// the <nick>/#swarm display conventions). Bound cryptographically
    /// into the swarm identity so joiners who decode the ID see the same
    /// name and a forged ID with a fake name fails to find peers.
    #[arg(long)]
    pub name: Option<SwarmName>,

    /// Use the public network (cross-machine via the relay + mDNS/DHT
    /// lookups). Omitted ⇒ private (loopback only, the default).
    /// Lookup selection lives in the shared
    /// `--mdns`/`--dht`/`--relay` allowlist flags.
    #[arg(long, default_value_t = false)]
    pub public: bool,

    /// Optional nickname (random word-word if not provided). A custom
    /// nickname is 1..=32 UTF-8 characters, excluding control chars,
    /// whitespace, and any of / \ < > #. Symmetric with `ahs join
    /// --nickname`.
    #[arg(long)]
    pub nickname: Option<Nickname>,

    /// List this swarm in a directory so others can find it with
    /// `ahs discover` — no `ahs…` id to share. Optional-value, like
    /// `--relay`: absent ⇒ unlisted; bare `--advertise` ⇒ the default
    /// `global` directory; `--advertise <directory>` ⇒ that named directory.
    /// Requires `--public` (a directory listing only makes sense for a
    /// cross-machine swarm). Note: advertising broadcasts the full join
    /// token, so the swarm becomes open to anyone discovering that directory.
    /// Absent ⇒ `None`; bare ⇒ `Some(None)`; valued ⇒ `Some(Some(directory))`.
    #[arg(long, num_args(0..=1))]
    #[expect(
        clippy::option_option,
        reason = "clap optional-value flag: absent/bare/valued are three distinct directory states (see DirectorySelection)"
    )]
    pub advertise: Option<Option<SwarmName>>,
}

impl CreateOpts {
    /// Resolve the `--advertise` flag's absent/bare/valued shape into a
    /// [`DirectorySelection`] (mirrors `LookupArgs::to_set` for `--relay`).
    fn advertise_selection(&self) -> DirectorySelection {
        match &self.advertise {
            None => DirectorySelection::Unset,
            Some(None) => DirectorySelection::Default,
            Some(Some(directory)) => DirectorySelection::Named(directory.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{
        Cli, Commands, DirectorySelection, Nickname, RelaySelection, SwarmMode, SwarmName,
    };

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
            Commands::Join { .. }
            | Commands::Msg { .. }
            | Commands::Poll { .. }
            | Commands::Ping { .. }
            | Commands::Discover { .. }
            | Commands::Mcp => {
                panic!("expected Create command")
            }
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
            Commands::Join { .. }
            | Commands::Msg { .. }
            | Commands::Poll { .. }
            | Commands::Ping { .. }
            | Commands::Discover { .. }
            | Commands::Mcp => {
                panic!("expected Create command")
            }
        }
    }

    #[test]
    fn create_opts_name_optional() {
        let cli = Cli::parse_from(["ahs", "create"]);
        match cli.command {
            Commands::Create { opts } => assert_eq!(opts.name, None),
            Commands::Join { .. }
            | Commands::Msg { .. }
            | Commands::Poll { .. }
            | Commands::Ping { .. }
            | Commands::Discover { .. }
            | Commands::Mcp => {
                panic!("expected Create command")
            }
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
    fn relay_flag_absent_bare_and_valued() {
        fn relay_of(args: &[&str]) -> RelaySelection {
            match Cli::parse_from(args).command {
                Commands::Create { opts } => opts.shared.lookups.to_set().relay,
                Commands::Join { .. }
                | Commands::Msg { .. }
                | Commands::Poll { .. }
                | Commands::Ping { .. }
                | Commands::Discover { .. }
                | Commands::Mcp => panic!("expected Create"),
            }
        }
        assert_eq!(
            relay_of(&["ahs", "create", "--public"]),
            RelaySelection::Unset,
            "absent ⇒ Unset"
        );
        assert_eq!(
            relay_of(&["ahs", "create", "--public", "--relay"]),
            RelaySelection::Default,
            "bare ⇒ Default (pinned)"
        );
        assert_eq!(
            relay_of(&[
                "ahs",
                "create",
                "--public",
                "--relay",
                "https://relay.example"
            ]),
            RelaySelection::Custom("https://relay.example".parse().unwrap()),
            "valued ⇒ Custom"
        );
    }

    #[test]
    fn advertise_flag_absent_bare_and_valued() {
        fn advertise_of(args: &[&str]) -> DirectorySelection {
            match Cli::parse_from(args).command {
                Commands::Create { opts } => opts.advertise_selection(),
                Commands::Join { .. }
                | Commands::Msg { .. }
                | Commands::Poll { .. }
                | Commands::Ping { .. }
                | Commands::Discover { .. }
                | Commands::Mcp => panic!("expected Create"),
            }
        }
        assert_eq!(
            advertise_of(&["ahs", "create", "--public"]),
            DirectorySelection::Unset,
            "absent ⇒ Unset (unlisted)"
        );
        assert_eq!(
            advertise_of(&["ahs", "create", "--public", "--advertise"]),
            DirectorySelection::Default,
            "bare ⇒ Default (global directory)"
        );
        assert_eq!(
            advertise_of(&["ahs", "create", "--public", "--advertise", "gamedev"]),
            DirectorySelection::Named(SwarmName::new("gamedev").unwrap()),
            "valued ⇒ Named directory"
        );
    }

    #[test]
    fn discover_parses_directory() {
        fn directory_of(args: &[&str]) -> Option<SwarmName> {
            match Cli::parse_from(args).command {
                Commands::Discover { directory, .. } => directory,
                Commands::Create { .. }
                | Commands::Join { .. }
                | Commands::Msg { .. }
                | Commands::Poll { .. }
                | Commands::Ping { .. }
                | Commands::Mcp => panic!("expected Discover"),
            }
        }
        // Bare discover ⇒ no explicit directory (defaults to global downstream).
        assert!(directory_of(&["ahs", "discover"]).is_none());
        // `--directory` is decoded into a SwarmName.
        assert_eq!(
            directory_of(&["ahs", "discover", "--directory", "gamedev"])
                .as_ref()
                .map(SwarmName::as_str),
            Some("gamedev")
        );
    }

    #[test]
    fn discover_mdns_resolves_to_mdns_only_lookups() {
        use crate::protocol::swarm::{RelayChoice, resolve_lookups};
        // `ahs discover --mdns` ⇒ the directory session uses mDNS only
        // (the same allowlist rule create/join apply to the swarm).
        let opts = match Cli::parse_from(["ahs", "discover", "--directory", "x", "--mdns"]).command
        {
            Commands::Discover { opts, .. } => opts,
            Commands::Create { .. }
            | Commands::Join { .. }
            | Commands::Msg { .. }
            | Commands::Poll { .. }
            | Commands::Ping { .. }
            | Commands::Mcp => panic!("expected Discover"),
        };
        let lookups = resolve_lookups(SwarmMode::Public, opts.shared.lookups.to_set()).unwrap();
        assert!(lookups.mdns && !lookups.dht);
        assert_eq!(lookups.relay, RelayChoice::Disabled);
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
            Commands::Join { .. }
            | Commands::Msg { .. }
            | Commands::Poll { .. }
            | Commands::Ping { .. }
            | Commands::Discover { .. }
            | Commands::Mcp => {
                panic!("expected Create command")
            }
        }
    }

    #[test]
    fn parse_keys_recognizes_arrows_enter_and_quit() {
        use super::{Key, parse_keys};
        // Arrow keys arrive as a CSI sequence, usually in one read.
        assert_eq!(parse_keys(b"\x1b[A"), vec![Key::Up]);
        assert_eq!(parse_keys(b"\x1b[B"), vec![Key::Down]);
        // Enter (CR or LF), vim j/k, q, and ctrl-c.
        assert_eq!(parse_keys(b"\r"), vec![Key::Enter]);
        assert_eq!(parse_keys(b"\n"), vec![Key::Enter]);
        assert_eq!(parse_keys(b"j"), vec![Key::Down]);
        assert_eq!(parse_keys(b"k"), vec![Key::Up]);
        assert_eq!(parse_keys(b"q"), vec![Key::Quit]);
        assert_eq!(parse_keys(&[0x03]), vec![Key::Quit]);
        // A lone ESC is a quit; other CSI sequences (e.g. →) are ignored.
        assert_eq!(parse_keys(b"\x1b"), vec![Key::Quit]);
        assert_eq!(parse_keys(b"\x1b[C"), vec![]);
        // Unknown bytes ignored; multiple keys in one chunk all parse.
        assert_eq!(parse_keys(b"x\x1b[Bq"), vec![Key::Down, Key::Quit]);
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
        /// whitespace, and any of / \ < > #.
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

    /// Ping all peers and have the daemon measure RTT. Fire-and-forget:
    /// the `ping_report` arrives on the running create/join daemon's
    /// `--output json` stream, not on this command's stdout.
    Ping {
        /// Swarm identifier (ahs...)
        #[arg(long)]
        swarm: SwarmId,

        /// Nickname of the local agent (must have a running join/create session)
        #[arg(long)]
        nickname: Nickname,
    },

    /// Browse swarms advertising themselves in a directory.
    ///
    /// Joins the directory and shows a live list of swarms
    /// created with `--advertise`. Interactive (default): pick a number
    /// to join. `--no-interactive` / `--output json`: stream
    /// `swarm_found` / `swarm_lost` JSON lines for an agent to act on.
    Discover {
        /// The directory to browse. Omit for the well-known `global`
        /// directory. Must match the directory publishers passed to
        /// `--advertise`.
        #[arg(long)]
        directory: Option<SwarmName>,

        #[command(flatten)]
        opts: JoinOpts,
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
        Commands::Ping { swarm, nickname } => ping(&swarm, &nickname).await,
        Commands::Discover { directory, opts } => discover(directory, opts).await,
        Commands::Mcp => crate::mcp::run().await,
    }
}

/// Build the output sink, resolve the author, set up the swarm, and
/// run the event loop. The shared spine of `create` and `join`.
async fn run_session(
    kind: SetupKind,
    lookups: LookupOpts,
    shared: SharedServerOpts,
    nickname: Option<Nickname>,
    advertise: DirectorySelection,
) -> Result<()> {
    let author = nickname.unwrap_or_else(Nickname::random);
    let out = Output::new(
        shared.output.into(),
        shared.filter_self,
        Some(author.as_str().to_owned()),
    );
    let mut cfg = setup_swarm(
        kind,
        author,
        !shared.no_interactive,
        shared.max_peers,
        shared.state_file,
        lookups.clone(),
        out,
    )
    .await?;
    // Advertising (`create --advertise`): start the re-broadcast task on
    // the swarm's own lookups. The handle is held for the session's
    // lifetime — on the CLI the process exits (via signal) before it
    // would drop, which tears the task down.
    let _advertiser = advertise
        .directory()
        .map(|directory| spawn_advertiser(&mut cfg, directory, lookups));
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
    let lookups = resolve_lookups(mode, opts.shared.lookups.to_set())?;
    let advertise = opts.advertise_selection();
    // `--advertise` lists the swarm publicly, so it requires `--public`
    // (rejected before any setup work — never a silent no-op).
    validate_advertise(mode, &advertise)?;
    let name = opts.name.unwrap_or_else(SwarmName::random);
    let kind = SetupKind::Create { mode, name };
    run_session(kind, lookups, opts.shared, opts.nickname, advertise).await
}

/// Join an existing swarm by its identifier (ahs...), a domain, or a
/// supported git repo URL. The network mode is decoded from the id;
/// lookup flags are per-process (the joiner opts in itself).
async fn join(swarm_input: &str, nickname: Option<Nickname>, opts: JoinOpts) -> Result<()> {
    let swarm: Swarm = resolver::resolve(swarm_input).await?;
    let lookups = resolve_lookups(swarm.mode, opts.shared.lookups.to_set())?;
    // `join` never advertises — that is a create-time decision.
    run_session(
        SetupKind::Join { swarm },
        lookups,
        opts.shared,
        nickname,
        DirectorySelection::Unset,
    )
    .await
}

#[derive(Deserialize)]
struct MsgResponse {
    ok: bool,
    id: Option<String>,
    error: Option<String>,
    #[serde(default)]
    rate_limited: bool,
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

    // A rate-limited send is a deliberate drop, not a failure — surface it
    // distinctly (still non-zero, so scripts see the message wasn't sent).
    if parsed.rate_limited {
        anyhow::bail!("rate limit exceeded — message not sent");
    }
    if !parsed.ok {
        anyhow::bail!(
            "{}",
            parsed.error.unwrap_or_else(|| "unknown error".to_string())
        );
    }

    let id = parsed.id.unwrap_or_default();
    // `msg` has no `--output` flag — always the human confirmation.
    // No nickname is rendered here (only `message posted` + id).
    let out = Output::new(OutputMode::Human, false, None);
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

/// Arm an RTT round on the running daemon. Fire-and-forget: the daemon
/// acks immediately and emits the `ping_report` on its own
/// `--output json` stream once the collection window closes.
async fn ping(swarm: &SwarmId, nickname: &Nickname) -> Result<()> {
    let cmd = IpcCommand::Ping {
        swarm: swarm.clone(),
    };
    let resp = ipc::send(&cmd, nickname).await?;
    let parsed: MsgResponse = serde_json::from_str(&resp)?;
    if !parsed.ok {
        anyhow::bail!(
            "{}",
            parsed.error.unwrap_or_else(|| "unknown error".to_string())
        );
    }
    Ok(())
}

/// Browse a directory. Human mode renders a live picker that
/// hands off to `join` on selection; `--no-interactive` / `--output
/// json` streams `swarm_found`/`swarm_lost` JSON lines instead (the
/// agent picks and joins by id itself). The lookup flags / nickname
/// in `opts` are reused for the eventual join.
async fn discover(directory: Option<SwarmName>, opts: JoinOpts) -> Result<()> {
    let directory_label = directory
        .as_ref()
        .map_or_else(|| "global".to_owned(), |name| name.as_str().to_owned());
    // The directory session uses the same `--mdns/--dht/--relay` lookups
    // the eventual join (below) will use, resolved against the directory's
    // mode (public in normal use; private under the test hook).
    let lookups = resolve_lookups(
        crate::directory::directory_mode(),
        opts.shared.lookups.to_set(),
    )?;
    let mut discoverer =
        Directory::open_with_lookups(directory.map(|name| name.as_str().to_owned()), lookups)
            .await?;
    let mut events = discoverer
        .events()
        .expect("discoverer events receiver is available exactly once");

    // Human + a real terminal ⇒ the live arrow-key picker; otherwise
    // (agent / piped) stream JSON changes.
    let want_picker = !opts.shared.no_interactive && opts.shared.output == OutputFormat::Human;
    if want_picker {
        match run_picker(&directory_label, &discoverer, &mut events).await {
            PickerOutcome::Selected(id) => {
                let _ = discoverer.close().await;
                return join(&id, None, opts).await;
            }
            PickerOutcome::Quit => {
                let _ = discoverer.close().await;
                return Ok(());
            }
            // No TTY for raw mode — fall through to the streaming view.
            PickerOutcome::Unsupported => {}
        }
    }

    // Agent / non-TTY mode: one JSON line per directory change until ctrl-c.
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            received = events.recv() => match received {
                Some(change) => println!("{}", discover_event_json(&change)),
                None => break,
            },
        }
    }
    let _ = discoverer.close().await;
    Ok(())
}

/// One directory change as a JSON line for `ahs discover --output json`.
/// `Found`/`Updated` both surface as `swarm_found` (upsert semantics —
/// the agent treats a re-ad as a refresh); a departure is `swarm_lost`.
fn discover_event_json(event: &DirectoryEvent) -> String {
    let value = match event {
        DirectoryEvent::Found(listing) | DirectoryEvent::Updated(listing) => serde_json::json!({
            "event": "swarm_found",
            "swarm": listing.swarm.as_str(),
            "name": listing.name,
            "mode": if listing.public { "public" } else { "private" },
            "peers": listing.peers,
        }),
        DirectoryEvent::Lost(swarm) => serde_json::json!({
            "event": "swarm_lost",
            "swarm": swarm.as_str(),
        }),
    };
    value.to_string()
}

/// What the interactive picker resolved to.
enum PickerOutcome {
    /// A swarm was chosen — carries its full `ahs…` id.
    Selected(String),
    /// User quit (`q` / esc / ctrl-c) or the directory closed.
    Quit,
    /// Raw terminal mode is unavailable (stdin isn't a TTY); the caller
    /// should fall back to the streaming view.
    Unsupported,
}

/// A keypress the picker acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Up,
    Down,
    Enter,
    Quit,
}

/// Clear screen + home cursor (a TTY control, not color — always on
/// since the picker only runs under a TTY). Swarm-name color reuses the
/// shared `output::style` constants, gated on `NO_COLOR` like the rest.
const PICKER_CLEAR: &str = "\x1b[2J\x1b[1;1H";

/// Run the live arrow-key picker until the user selects/quits or the
/// directory closes. Redraws on every directory change; `↑`/`↓` (and
/// `j`/`k`) move, `enter` joins the highlighted swarm, `q`/esc/ctrl-c
/// quit. Returns [`PickerOutcome::Unsupported`] when stdin isn't a TTY.
async fn run_picker(
    directory_label: &str,
    discoverer: &Directory,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<DirectoryEvent>,
) -> PickerOutcome {
    let Some(raw) = RawMode::enable() else {
        return PickerOutcome::Unsupported;
    };
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (mut key_rx, key_handle) = spawn_key_reader(Arc::clone(&stop));

    let mut selected: usize = 0;
    // Cached listing set — refreshed only on a directory event (which is
    // the only thing that changes it). A keypress just moves `selected`,
    // so it reuses this Vec instead of re-cloning + re-sorting the whole
    // directory on every arrow press.
    let mut listings = discoverer.snapshot();
    render_picker(directory_label, &listings, selected);

    let outcome = loop {
        tokio::select! {
            key = key_rx.recv() => {
                let Some(key) = key else { break PickerOutcome::Quit };
                match key {
                    Key::Up => selected = selected.saturating_sub(1),
                    Key::Down if selected + 1 < listings.len() => selected += 1,
                    Key::Down => {}
                    Key::Enter => {
                        if let Some(listing) = listings.get(selected) {
                            break PickerOutcome::Selected(listing.swarm.as_str().to_owned());
                        }
                    }
                    Key::Quit => break PickerOutcome::Quit,
                }
                render_picker(directory_label, &listings, selected);
            }
            change = events.recv() => {
                if change.is_none() {
                    break PickerOutcome::Quit; // directory closed
                }
                listings = discoverer.snapshot();
                selected = selected.min(listings.len().saturating_sub(1));
                render_picker(directory_label, &listings, selected);
            }
        }
    };

    // Stop the reader *before* restoring the tty: it must exit while
    // still in VTIME-poll mode, or its next read would block on a line
    // and steal stdin from the `join` we hand off to.
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = key_handle.join();
    drop(raw);
    print_clear();
    outcome
}

/// Redraw the picker: lowercase chrome, the directory + each swarm name in
/// yellow, the full `ahs…` id, peer count, and an ISO-8601 UTC
/// first-seen timestamp. `selected` is the highlighted row. Output
/// post-processing (ONLCR) is left on, so `\n` still becomes CRLF in
/// raw mode.
fn render_picker(directory_label: &str, listings: &[SwarmListing], selected: usize) {
    use crate::output::style;
    use std::fmt::Write as _;

    // Reuse the shared swarm-name color, gated on a TTY + `NO_COLOR`
    // like every other colored path. Empty strings when off.
    let (yellow, reset) = if crate::output::stdout_color() {
        (style::SWARM, style::RESET)
    } else {
        ("", "")
    };

    let mut out = String::from(PICKER_CLEAR);
    let _ = write!(
        out,
        "discovering {yellow}#{directory_label}{reset} directory\n\n"
    );
    if listings.is_empty() {
        out.push_str("  (waiting for swarms…)\n");
    } else {
        for (index, listing) in listings.iter().enumerate() {
            let marker = if index == selected { "❯" } else { " " };
            let bold = if index == selected && !yellow.is_empty() {
                style::BOLD
            } else {
                ""
            };
            let _ = writeln!(
                out,
                "{marker} {bold}{yellow}#{}{reset}  {}  {}  {}",
                listing.name,
                listing.swarm.as_str(),
                listing.peers,
                crate::util::clock::iso8601_utc(listing.first_seen_unix),
            );
        }
    }
    out.push_str("\n↑/↓ move · enter join · q quit\n");
    print_str(&out);
}

/// Clear the screen (used when leaving the picker for a clean handoff).
fn print_clear() {
    print_str(PICKER_CLEAR);
}

/// Write a string straight to stdout and flush — the picker controls the
/// screen itself rather than going through `println!`.
fn print_str(text: &str) {
    use std::io::Write as _;
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

/// RAII raw-terminal guard for the picker: disables canonical mode,
/// echo, and signal generation (so ctrl-c arrives as a byte we handle),
/// and sets a ~100 ms read poll (`VMIN=0`/`VTIME=1`) so the reader
/// thread can observe its stop flag. Restores the original settings on
/// drop. `None` when stdin isn't a TTY.
struct RawMode {
    original: libc::termios,
}

impl RawMode {
    #[expect(
        unsafe_code,
        reason = "libc termios FFI to put the tty in raw mode for the discover picker; no safe wrapper"
    )]
    fn enable() -> Option<Self> {
        // SAFETY: a zeroed `termios` is a valid buffer for `tcgetattr` to
        // fill; all calls target the process's own stdin fd.
        unsafe {
            let mut termios: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &raw mut termios) != 0 {
                return None;
            }
            let original = termios;
            termios.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
            termios.c_cc[libc::VMIN] = 0;
            termios.c_cc[libc::VTIME] = 1;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw const termios) != 0 {
                return None;
            }
            Some(Self { original })
        }
    }
}

impl Drop for RawMode {
    #[expect(
        unsafe_code,
        reason = "libc termios FFI to restore the tty when the picker exits; no safe wrapper"
    )]
    fn drop(&mut self) {
        // SAFETY: restoring the exact `termios` captured in `enable`.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw const self.original);
        }
    }
}

/// Spawn a thread that reads stdin in raw mode and forwards parsed
/// [`Key`]s over a channel. It polls `stop` (via the `VTIME` read
/// timeout) and exits within ~100 ms once set — so it releases stdin
/// before the picker hands control to `join`.
fn spawn_key_reader(
    stop: Arc<std::sync::atomic::AtomicBool>,
) -> (
    tokio::sync::mpsc::UnboundedReceiver<Key>,
    std::thread::JoinHandle<()>,
) {
    let (key_tx, key_rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            match read_stdin(&mut buf) {
                Some(0) => {} // VTIME timeout, no input — re-check `stop`
                Some(count) => {
                    for key in parse_keys(&buf[..count]) {
                        if key_tx.send(key).is_err() {
                            return;
                        }
                    }
                }
                None => return, // read error
            }
        }
    });
    (key_rx, handle)
}

/// One raw `read(2)` from stdin. `Some(0)` on the `VTIME` poll timeout,
/// `Some(n)` with bytes read, `None` on error. Bypasses std's buffered
/// stdin so the `VMIN`/`VTIME` poll behaves as configured.
#[expect(
    unsafe_code,
    reason = "libc read(2) on STDIN_FILENO to honor the raw-mode VMIN/VTIME poll; std's buffered stdin would not"
)]
fn read_stdin(buf: &mut [u8]) -> Option<usize> {
    // SAFETY: `read` writes at most `buf.len()` bytes into the valid
    // mutable slice `buf`.
    let count = unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr().cast(), buf.len()) };
    usize::try_from(count).ok()
}

/// Parse a raw input chunk into [`Key`]s: arrow `↑`/`↓` (CSI `A`/`B`),
/// `enter`, `q`/esc/ctrl-c as quit, and vim `j`/`k`. Unknown bytes and
/// other escape sequences are ignored.
fn parse_keys(bytes: &[u8]) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            0x1b => {
                // CSI: ESC '[' final-byte. Recognize the arrows; skip
                // any other sequence. A lone ESC is a quit.
                if bytes.get(index + 1) == Some(&b'[') {
                    match bytes.get(index + 2) {
                        Some(b'A') => keys.push(Key::Up),
                        Some(b'B') => keys.push(Key::Down),
                        _ => {}
                    }
                    index += 3;
                    continue;
                }
                keys.push(Key::Quit);
            }
            b'\r' | b'\n' => keys.push(Key::Enter),
            b'q' | b'Q' | 0x03 => keys.push(Key::Quit),
            b'j' => keys.push(Key::Down),
            b'k' => keys.push(Key::Up),
            _ => {}
        }
        index += 1;
    }
    keys
}
