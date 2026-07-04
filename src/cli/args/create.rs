//! `create` command args: mint and join a new swarm.

use clap::Parser;

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
    /// minted if omitted. 1..=32 UTF-8 characters (any script/emoji),
    /// excluding control characters, whitespace, and any of < > # (reserved
    /// for the `<nick>`/#swarm display conventions). Unlike a nickname, a swarm
    /// name may contain `/` (it is never a filename), so it can be a URL. Bound
    /// cryptographically into the swarm identity so joiners who decode the ID
    /// see the same name and a forged ID with a fake name fails to find peers.
    #[arg(long)]
    pub name: Option<SwarmName>,

    /// Make the swarm reachable across machines — sugar for the all-on
    /// lookup preset (mDNS + DHT + the default relay ladder). Omitted ⇒
    /// loopback only (the default). Refine with the `--mdns`/`--dht`/
    /// `--relay` flags; all of it is baked into the swarm id.
    #[arg(long, default_value_t = false)]
    pub public: bool,

    /// Optional nickname (random word-word if not provided). A custom
    /// nickname is 1..=32 UTF-8 characters, excluding control chars,
    /// whitespace, and any of / \ < > #. Symmetric with `ahsw join
    /// --nickname`.
    #[arg(long)]
    pub nickname: Option<Nickname>,

    /// List this swarm in a directory so others can find it with
    /// `ahsw discover` — no `🐝…` id to share. Optional-value, like
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

    /// Protect the swarm with a password: the id alone no longer admits —
    /// joiners must present the password (so a passworded swarm is safe to
    /// `--advertise`). The id carries only a one-way verifier, never the
    /// password. Bare `--password` prompts hidden on the terminal;
    /// `--password=<pw>` passes it inline (visible in `ps` and shell
    /// history — prefer the prompt when a human types it).
    #[arg(long, num_args(0..=1), require_equals = true)]
    #[expect(
        clippy::option_option,
        reason = "clap optional-value flag: absent/bare/valued are three distinct password states"
    )]
    pub password: Option<Option<String>>,
}

impl CreateOpts {
    /// Resolve the `--advertise` flag's absent/bare/valued shape into a
    /// [`DirectorySelection`] (mirrors `LookupArgs::to_set` for `--relay`).
    pub(crate) fn advertise_selection(&self) -> DirectorySelection {
        DirectorySelection::from_flag(self.advertise.clone())
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
        let cli = Cli::parse_from(["ahsw", "create", "--name", "team", "--nickname", "my-nick"]);
        match cli.command {
            Commands::Create { opts } => {
                assert_eq!(opts.name.as_ref().map(SwarmName::as_str), Some("team"));
                assert_eq!(
                    opts.nickname.as_ref().map(Nickname::as_str),
                    Some("my-nick")
                );
            }
            Commands::Join { .. }
            | Commands::Forum { .. }
            | Commands::Poll { .. }
            | Commands::Ping { .. }
            | Commands::Pipe { .. }
            | Commands::Port { .. }
            | Commands::File { .. }
            | Commands::Sh { .. }
            | Commands::Mount { .. }
            | Commands::Discover { .. }
            | Commands::Mcp { .. }
            | Commands::Man
            | Commands::Peers { .. }
            | Commands::State { .. }
            | Commands::Meta { .. }
            | Commands::Card { .. }
            | Commands::A2a { .. }
            | Commands::Ready { .. }
            | Commands::Plug { .. }
            | Commands::Unplug { .. }
            | Commands::Doctor { .. }
            | Commands::Leave { .. }
            | Commands::Session { .. } => {
                panic!("expected Create command")
            }
        }
    }

    #[test]
    fn create_opts_without_nickname() {
        let cli = Cli::parse_from(["ahsw", "create", "--name", "team"]);
        match cli.command {
            Commands::Create { opts } => {
                assert_eq!(opts.name.as_ref().map(SwarmName::as_str), Some("team"));
                assert_eq!(opts.nickname, None);
            }
            Commands::Join { .. }
            | Commands::Forum { .. }
            | Commands::Poll { .. }
            | Commands::Ping { .. }
            | Commands::Pipe { .. }
            | Commands::Port { .. }
            | Commands::File { .. }
            | Commands::Sh { .. }
            | Commands::Mount { .. }
            | Commands::Discover { .. }
            | Commands::Mcp { .. }
            | Commands::Man
            | Commands::Peers { .. }
            | Commands::State { .. }
            | Commands::Meta { .. }
            | Commands::Card { .. }
            | Commands::A2a { .. }
            | Commands::Ready { .. }
            | Commands::Plug { .. }
            | Commands::Unplug { .. }
            | Commands::Doctor { .. }
            | Commands::Leave { .. }
            | Commands::Session { .. } => {
                panic!("expected Create command")
            }
        }
    }

    #[test]
    fn create_opts_name_optional() {
        let cli = Cli::parse_from(["ahsw", "create"]);
        match cli.command {
            Commands::Create { opts } => assert_eq!(opts.name, None),
            Commands::Join { .. }
            | Commands::Forum { .. }
            | Commands::Poll { .. }
            | Commands::Ping { .. }
            | Commands::Pipe { .. }
            | Commands::Port { .. }
            | Commands::File { .. }
            | Commands::Sh { .. }
            | Commands::Mount { .. }
            | Commands::Discover { .. }
            | Commands::Mcp { .. }
            | Commands::Man
            | Commands::Peers { .. }
            | Commands::State { .. }
            | Commands::Meta { .. }
            | Commands::Card { .. }
            | Commands::A2a { .. }
            | Commands::Ready { .. }
            | Commands::Plug { .. }
            | Commands::Unplug { .. }
            | Commands::Doctor { .. }
            | Commands::Leave { .. }
            | Commands::Session { .. } => {
                panic!("expected Create command")
            }
        }
    }

    #[test]
    fn create_opts_rejects_invalid_name() {
        assert!(Cli::try_parse_from(["ahsw", "create", "--name", ""]).is_err());
        assert!(
            Cli::try_parse_from(["ahsw", "create", "--name", "has space"]).is_err(),
            "whitespace must reject"
        );
        assert!(
            Cli::try_parse_from(["ahsw", "create", "--name", "a#b"]).is_err(),
            "the #swarm marker must reject"
        );
        assert!(
            Cli::try_parse_from(["ahsw", "create", "--name", "a/b"]).is_ok(),
            "a swarm name may contain a path separator (it is never a filename)"
        );
        assert!(
            Cli::try_parse_from(["ahsw", "create", "--name", &"a".repeat(33)]).is_err(),
            "33 chars must reject"
        );
    }

    #[test]
    fn advertise_flag_absent_bare_and_valued() {
        fn advertise_of(args: &[&str]) -> DirectorySelection {
            match Cli::parse_from(args).command {
                Commands::Create { opts } => opts.advertise_selection(),
                Commands::Join { .. }
                | Commands::Forum { .. }
                | Commands::Poll { .. }
                | Commands::Ping { .. }
                | Commands::Pipe { .. }
                | Commands::Port { .. }
                | Commands::File { .. }
                | Commands::Sh { .. }
                | Commands::Mount { .. }
                | Commands::Discover { .. }
                | Commands::Mcp { .. }
                | Commands::Man
                | Commands::Peers { .. }
                | Commands::State { .. }
                | Commands::Meta { .. }
                | Commands::Card { .. }
                | Commands::A2a { .. }
                | Commands::Ready { .. }
                | Commands::Plug { .. }
                | Commands::Unplug { .. }
                | Commands::Doctor { .. }
                | Commands::Leave { .. }
                | Commands::Session { .. } => panic!("expected Create"),
            }
        }
        assert_eq!(
            advertise_of(&["ahsw", "create", "--public"]),
            DirectorySelection::Unset,
            "absent ⇒ Unset (unlisted)"
        );
        assert_eq!(
            advertise_of(&["ahsw", "create", "--public", "--advertise"]),
            DirectorySelection::Default,
            "bare ⇒ Default (global directory)"
        );
        assert_eq!(
            advertise_of(&["ahsw", "create", "--public", "--advertise", "gamedev"]),
            DirectorySelection::Named(SwarmName::new("gamedev").unwrap()),
            "valued ⇒ Named directory"
        );
    }

    #[test]
    fn password_flag_absent_bare_and_valued() {
        #[expect(
            clippy::option_option,
            reason = "mirrors the clap optional-value flag under test"
        )]
        fn password_of(args: &[&str]) -> Option<Option<String>> {
            match Cli::parse_from(args).command {
                Commands::Create { opts } => opts.password,
                Commands::Join { .. }
                | Commands::Forum { .. }
                | Commands::Poll { .. }
                | Commands::Ping { .. }
                | Commands::Pipe { .. }
                | Commands::Port { .. }
                | Commands::File { .. }
                | Commands::Discover { .. }
                | Commands::Mcp { .. }
                | Commands::Man
                | Commands::Peers { .. }
                | Commands::State { .. }
                | Commands::Meta { .. }
                | Commands::Card { .. }
                | Commands::A2a { .. }
                | Commands::Ready { .. }
                | Commands::Plug { .. }
                | Commands::Unplug { .. }
                | Commands::Sh { .. }
                | Commands::Mount { .. }
                | Commands::Doctor { .. }
                | Commands::Leave { .. }
                | Commands::Session { .. } => panic!("expected Create"),
            }
        }
        assert_eq!(password_of(&["ahsw", "create"]), None, "absent ⇒ None");
        assert_eq!(
            password_of(&["ahsw", "create", "--password"]),
            Some(None),
            "bare ⇒ prompt"
        );
        assert_eq!(
            password_of(&["ahsw", "create", "--password=hunter2"]),
            Some(Some("hunter2".to_owned())),
            "valued ⇒ inline"
        );
        // require_equals is load-bearing: a space-separated value must NOT
        // be swallowed as the password (it would eat positionals on the
        // connect-style commands).
        assert!(Cli::try_parse_from(["ahsw", "create", "--password", "hunter2"]).is_err());
    }

    #[test]
    fn create_opts_nickname_with_other_flags() {
        let cli = Cli::parse_from([
            "ahsw",
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
            | Commands::Forum { .. }
            | Commands::Poll { .. }
            | Commands::Ping { .. }
            | Commands::Pipe { .. }
            | Commands::Port { .. }
            | Commands::File { .. }
            | Commands::Sh { .. }
            | Commands::Mount { .. }
            | Commands::Discover { .. }
            | Commands::Mcp { .. }
            | Commands::Man
            | Commands::Peers { .. }
            | Commands::State { .. }
            | Commands::Meta { .. }
            | Commands::Card { .. }
            | Commands::A2a { .. }
            | Commands::Ready { .. }
            | Commands::Plug { .. }
            | Commands::Unplug { .. }
            | Commands::Doctor { .. }
            | Commands::Leave { .. }
            | Commands::Session { .. } => {
                panic!("expected Create command")
            }
        }
    }
}
