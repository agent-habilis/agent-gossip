//! The command-line surface: the clap-derived argument structs/enums
//! (`Cli`, `Commands`, the per-command options) and the parse tests.
//! The imperative per-command logic lives in the parent [`super`] module.

use clap::{Parser, Subcommand, ValueEnum};
use iroh::RelayUrl;

use crate::output::OutputMode;
use crate::protocol::swarm::{DirectorySelection, LookupSet, RelaySelection, SwarmName};
use crate::protocol::{MessageBody, MessageId, Nickname, SwarmId};
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
    pub(super) fn to_set(&self) -> LookupSet {
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
    pub(super) fn advertise_selection(&self) -> DirectorySelection {
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

    use super::{Cli, Commands, DirectorySelection, Nickname, RelaySelection, SwarmName};

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
        use crate::protocol::swarm::{RelayChoice, SwarmMode, resolve_lookups};
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

/// `OutputFormat` (the clap value-enum) → the daemon's `OutputMode`.
impl From<OutputFormat> for OutputMode {
    fn from(fmt: OutputFormat) -> Self {
        match fmt {
            OutputFormat::Human => OutputMode::Human,
            OutputFormat::Json => OutputMode::Json,
        }
    }
}
