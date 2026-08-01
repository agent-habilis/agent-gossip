//! `create` command args: mint and join a new gossip.

use clap::Parser;

use crate::cli::password::PasswordFlag;
use agent_habilis_mesh::protocol::Nickname;
use agent_habilis_mesh::protocol::{DirectorySelection, MeshName};

use super::lookup::LookupArgs;
use super::shared::SharedServerOpts;

#[derive(Parser, Debug)]
pub(crate) struct CreateOpts {
    #[command(flatten)]
    pub shared: SharedServerOpts,

    /// Which lookup mechanisms this gossip uses (baked into its id, so
    /// every joiner inherits them). `create`-only: `join` decodes them.
    #[command(flatten)]
    pub lookups: LookupArgs,

    /// Human-readable gossip name. Optional — a random word-word name is
    /// minted if omitted. 1..=32 UTF-8 characters (any script/emoji),
    /// excluding control characters, whitespace, and any of < > # (reserved
    /// for the `<nick>`/#gossip display conventions). Unlike a nickname, a gossip
    /// name may contain `/` (it is never a filename), so it can be a URL. Bound
    /// cryptographically into the gossip identity so joiners who decode the ID
    /// see the same name and a forged ID with a fake name fails to find peers.
    #[arg(long)]
    pub name: Option<MeshName>,

    /// Make the gossip reachable across machines — sugar for the all-on
    /// lookup preset (mDNS + DHT + the default relay ladder). Omitted ⇒
    /// loopback only (the default). Refine with the `--mdns`/`--dht`/
    /// `--relay` flags; all of it is baked into the gossip id.
    #[arg(long, default_value_t = false)]
    pub public: bool,

    /// Optional nickname (random word-word if not provided). A custom
    /// nickname is 1..=32 UTF-8 characters, excluding control chars,
    /// whitespace, and any of / \ < > #. Symmetric with `agent-gossip join
    /// --nickname`.
    #[arg(long)]
    pub nickname: Option<Nickname>,

    /// List this gossip in a directory so others can find it with
    /// `agent-gossip discover` — no id to share. Optional-value, like
    /// `--relay`: absent ⇒ unlisted; bare `--advertise` ⇒ the default
    /// `global` directory; `--advertise <directory>` ⇒ that named directory.
    /// Requires `--public` (a directory listing only makes sense for a
    /// cross-machine gossip). Note: advertising broadcasts the full join
    /// token, so the gossip becomes open to anyone discovering that directory.
    /// Absent ⇒ unlisted; bare `--advertise` ⇒ the well-known `global`
    /// directory (the `default_missing_value`); valued ⇒ that named directory.
    #[arg(long, num_args(0..=1), default_missing_value = "global")]
    pub advertise: Option<MeshName>,

    /// Protect the gossip with a password: the id alone no longer admits —
    /// joiners must present the password (so a passworded gossip is safe to
    /// `--advertise`). The id carries only a one-way verifier, never the
    /// password. Pass it inline as `--password=<pw>` (a bare `--password` is
    /// an error: there is no terminal prompt).
    #[arg(long, num_args(0..=1), require_equals = true, default_missing_value = "\0")]
    pub password: Option<PasswordFlag>,

    /// Make the gossip invite-only: the bare id can no longer join —
    /// only a creator-minted invite can. The id carries the issuer public
    /// key (the mint authority), never the join secret; mint invites with
    /// `agent-gossip invite`. Combine with `--password` to also password-protect
    /// every minted invite.
    #[arg(long)]
    pub invite_only: bool,
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
    use crate::cli::password::PasswordFlag;
    use agent_habilis_mesh::protocol::Nickname;
    use agent_habilis_mesh::protocol::{DirectorySelection, MeshName};

    #[test]
    fn create_opts_with_nickname() {
        let cli = Cli::parse_from([
            "agent-gossip",
            "create",
            "--name",
            "team",
            "--nickname",
            "my-nick",
        ]);
        match cli.command {
            Commands::Create { opts } => {
                assert_eq!(opts.name.as_ref().map(MeshName::as_str), Some("team"));
                assert_eq!(
                    opts.nickname.as_ref().map(Nickname::as_str),
                    Some("my-nick")
                );
            }
            Commands::Join { .. }
            | Commands::Topic { .. }
            | Commands::Poll { .. }
            | Commands::Ping { .. }
            | Commands::Discover { .. }
            | Commands::Mcp { .. }
            | Commands::Man
            | Commands::Peers { .. }
            | Commands::State { .. }
            | Commands::Meta { .. }
            | Commands::Topology { .. }
            | Commands::A2a { .. }
            | Commands::Ready { .. }
            | Commands::Plug { .. }
            | Commands::Unplug { .. }
            | Commands::Doctor { .. }
            | Commands::Leave { .. }
            | Commands::Invite { .. }
            | Commands::Session { .. } => {
                panic!("expected Create command")
            }
        }
    }

    #[test]
    fn create_opts_without_nickname() {
        let cli = Cli::parse_from(["agent-gossip", "create", "--name", "team"]);
        match cli.command {
            Commands::Create { opts } => {
                assert_eq!(opts.name.as_ref().map(MeshName::as_str), Some("team"));
                assert_eq!(opts.nickname, None);
            }
            Commands::Join { .. }
            | Commands::Topic { .. }
            | Commands::Poll { .. }
            | Commands::Ping { .. }
            | Commands::Discover { .. }
            | Commands::Mcp { .. }
            | Commands::Man
            | Commands::Peers { .. }
            | Commands::State { .. }
            | Commands::Meta { .. }
            | Commands::Topology { .. }
            | Commands::A2a { .. }
            | Commands::Ready { .. }
            | Commands::Plug { .. }
            | Commands::Unplug { .. }
            | Commands::Doctor { .. }
            | Commands::Leave { .. }
            | Commands::Invite { .. }
            | Commands::Session { .. } => {
                panic!("expected Create command")
            }
        }
    }

    #[test]
    fn create_opts_name_optional() {
        let cli = Cli::parse_from(["agent-gossip", "create"]);
        match cli.command {
            Commands::Create { opts } => assert_eq!(opts.name, None),
            Commands::Join { .. }
            | Commands::Topic { .. }
            | Commands::Poll { .. }
            | Commands::Ping { .. }
            | Commands::Discover { .. }
            | Commands::Mcp { .. }
            | Commands::Man
            | Commands::Peers { .. }
            | Commands::State { .. }
            | Commands::Meta { .. }
            | Commands::Topology { .. }
            | Commands::A2a { .. }
            | Commands::Ready { .. }
            | Commands::Plug { .. }
            | Commands::Unplug { .. }
            | Commands::Doctor { .. }
            | Commands::Leave { .. }
            | Commands::Invite { .. }
            | Commands::Session { .. } => {
                panic!("expected Create command")
            }
        }
    }

    #[test]
    fn create_opts_rejects_invalid_name() {
        assert!(Cli::try_parse_from(["agent-gossip", "create", "--name", ""]).is_err());
        assert!(
            Cli::try_parse_from(["agent-gossip", "create", "--name", "has space"]).is_err(),
            "whitespace must reject"
        );
        assert!(
            Cli::try_parse_from(["agent-gossip", "create", "--name", "a#b"]).is_err(),
            "the #mesh marker must reject"
        );
        assert!(
            Cli::try_parse_from(["agent-gossip", "create", "--name", "a/b"]).is_ok(),
            "a mesh name may contain a path separator (it is never a filename)"
        );
        assert!(
            Cli::try_parse_from(["agent-gossip", "create", "--name", &"a".repeat(33)]).is_err(),
            "33 chars must reject"
        );
    }

    #[test]
    fn advertise_flag_absent_bare_and_valued() {
        fn advertise_of(args: &[&str]) -> DirectorySelection {
            match Cli::parse_from(args).command {
                Commands::Create { opts } => opts.advertise_selection(),
                Commands::Join { .. }
                | Commands::Topic { .. }
                | Commands::Poll { .. }
                | Commands::Ping { .. }
                | Commands::Discover { .. }
                | Commands::Mcp { .. }
                | Commands::Man
                | Commands::Peers { .. }
                | Commands::State { .. }
                | Commands::Meta { .. }
                | Commands::Topology { .. }
                | Commands::A2a { .. }
                | Commands::Ready { .. }
                | Commands::Plug { .. }
                | Commands::Unplug { .. }
                | Commands::Doctor { .. }
                | Commands::Leave { .. }
                | Commands::Invite { .. }
                | Commands::Session { .. } => panic!("expected Create"),
            }
        }
        assert_eq!(
            advertise_of(&["agent-gossip", "create", "--public"]),
            DirectorySelection::Unset,
            "absent ⇒ Unset (unlisted)"
        );
        assert_eq!(
            advertise_of(&["agent-gossip", "create", "--public", "--advertise"]),
            DirectorySelection::Named(MeshName::new("global").unwrap()),
            "bare ⇒ the global directory (default_missing_value)"
        );
        assert_eq!(
            advertise_of(&[
                "agent-gossip",
                "create",
                "--public",
                "--advertise",
                "gamedev"
            ]),
            DirectorySelection::Named(MeshName::new("gamedev").unwrap()),
            "valued ⇒ Named directory"
        );
    }

    #[test]
    fn password_flag_absent_bare_and_valued() {
        fn password_of(args: &[&str]) -> Option<PasswordFlag> {
            match Cli::parse_from(args).command {
                Commands::Create { opts } => opts.password,
                Commands::Join { .. }
                | Commands::Topic { .. }
                | Commands::Poll { .. }
                | Commands::Ping { .. }
                | Commands::Discover { .. }
                | Commands::Mcp { .. }
                | Commands::Man
                | Commands::Peers { .. }
                | Commands::State { .. }
                | Commands::Meta { .. }
                | Commands::Topology { .. }
                | Commands::A2a { .. }
                | Commands::Ready { .. }
                | Commands::Plug { .. }
                | Commands::Unplug { .. }
                | Commands::Doctor { .. }
                | Commands::Leave { .. }
                | Commands::Invite { .. }
                | Commands::Session { .. } => panic!("expected Create"),
            }
        }
        assert_eq!(
            password_of(&["agent-gossip", "create"]),
            None,
            "absent ⇒ None"
        );
        assert_eq!(
            password_of(&["agent-gossip", "create", "--password"]),
            Some(PasswordFlag::Bare),
            "bare ⇒ bare (rejected at resolve time)"
        );
        assert_eq!(
            password_of(&["agent-gossip", "create", "--password=hunter2"]),
            Some(PasswordFlag::Inline("hunter2".to_owned())),
            "valued ⇒ inline"
        );
        // require_equals is load-bearing: a space-separated value must NOT
        // be swallowed as the password (it would eat positionals on the
        // connect-style commands).
        assert!(Cli::try_parse_from(["agent-gossip", "create", "--password", "hunter2"]).is_err());
    }

    #[test]
    fn create_opts_nickname_with_other_flags() {
        let cli = Cli::parse_from([
            "agent-gossip",
            "create",
            "--name",
            "team",
            "--public",
            "--nickname",
            "custom-name",
        ]);
        match cli.command {
            Commands::Create { opts } => {
                assert_eq!(opts.name.as_ref().map(MeshName::as_str), Some("team"));
                assert_eq!(
                    opts.nickname.as_ref().map(Nickname::as_str),
                    Some("custom-name")
                );
                assert!(opts.public);
            }
            Commands::Join { .. }
            | Commands::Topic { .. }
            | Commands::Poll { .. }
            | Commands::Ping { .. }
            | Commands::Discover { .. }
            | Commands::Mcp { .. }
            | Commands::Man
            | Commands::Peers { .. }
            | Commands::State { .. }
            | Commands::Meta { .. }
            | Commands::Topology { .. }
            | Commands::A2a { .. }
            | Commands::Ready { .. }
            | Commands::Plug { .. }
            | Commands::Unplug { .. }
            | Commands::Doctor { .. }
            | Commands::Leave { .. }
            | Commands::Invite { .. }
            | Commands::Session { .. } => {
                panic!("expected Create command")
            }
        }
    }
}
