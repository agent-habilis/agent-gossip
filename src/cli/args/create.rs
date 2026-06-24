//! `create` command args: mint and join a new swarm.

use clap::Parser;

use crate::util::consts::RATE_LIMIT_PER_MIN;

use crate::protocol::Nickname;
use crate::protocol::swarm::{DirectorySelection, SwarmName};

use super::lookup::LookupArgs;
use super::shared::SharedServerOpts;

#[derive(Parser, Debug)]
pub(crate) struct CreateOpts {
    #[command(flatten)]
    pub shared: SharedServerOpts,

    /// Which lookup mechanisms this swarm uses (baked into its id, so
    /// every joiner inherits them). `create`-only: `join` decodes them.
    #[command(flatten)]
    pub lookups: LookupArgs,

    /// Human-readable swarm name. Optional — a random word-word name is
    /// minted if omitted. Same rules as a nickname: 1..=32 UTF-8
    /// characters (any script/emoji), excluding control characters,
    /// whitespace, and any of / \ < > # (the last three are reserved for
    /// the `<nick>`/#swarm display conventions). Bound cryptographically
    /// into the swarm identity so joiners who decode the ID see the same
    /// name and a forged ID with a fake name fails to find peers.
    #[arg(long)]
    pub name: Option<SwarmName>,

    /// Make the swarm reachable across machines — sugar for the all-on
    /// lookup preset (mDNS + DHT + the default relay ladder). Omitted ⇒
    /// loopback only (the default). Refine with the `--mdns`/`--dht`/
    /// `--relay` flags; all of it is baked into the swarm id.
    #[arg(long, default_value_t = false)]
    pub public: bool,

    /// Per-author messages-per-minute cap, baked into the swarm id and
    /// enforced swarm-wide (every joiner inherits it). `0` disables rate
    /// limiting entirely. Default 60.
    #[arg(long = "rate-limit", default_value_t = RATE_LIMIT_PER_MIN)]
    pub rate_limit: u16,

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
    pub(crate) fn advertise_selection(&self) -> DirectorySelection {
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

    use crate::cli::args::{Cli, Commands};
    use crate::protocol::Nickname;
    use crate::protocol::swarm::{DirectorySelection, SwarmName};

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
            | Commands::Mcp { .. }
            | Commands::Man
            | Commands::Exchange { .. }
            | Commands::Peers { .. }
            | Commands::Ready { .. }
            | Commands::Setup { .. }
            | Commands::Teardown { .. }
            | Commands::Status => {
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
            | Commands::Mcp { .. }
            | Commands::Man
            | Commands::Exchange { .. }
            | Commands::Peers { .. }
            | Commands::Ready { .. }
            | Commands::Setup { .. }
            | Commands::Teardown { .. }
            | Commands::Status => {
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
            | Commands::Mcp { .. }
            | Commands::Man
            | Commands::Exchange { .. }
            | Commands::Peers { .. }
            | Commands::Ready { .. }
            | Commands::Setup { .. }
            | Commands::Teardown { .. }
            | Commands::Status => {
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
    fn advertise_flag_absent_bare_and_valued() {
        fn advertise_of(args: &[&str]) -> DirectorySelection {
            match Cli::parse_from(args).command {
                Commands::Create { opts } => opts.advertise_selection(),
                Commands::Join { .. }
                | Commands::Msg { .. }
                | Commands::Poll { .. }
                | Commands::Ping { .. }
                | Commands::Discover { .. }
                | Commands::Mcp { .. }
                | Commands::Man
                | Commands::Exchange { .. }
                | Commands::Peers { .. }
                | Commands::Ready { .. }
                | Commands::Setup { .. }
                | Commands::Teardown { .. }
                | Commands::Status => panic!("expected Create"),
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
            | Commands::Mcp { .. }
            | Commands::Man
            | Commands::Exchange { .. }
            | Commands::Peers { .. }
            | Commands::Ready { .. }
            | Commands::Setup { .. }
            | Commands::Teardown { .. }
            | Commands::Status => {
                panic!("expected Create command")
            }
        }
    }
}
