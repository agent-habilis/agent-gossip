//! `poll` command args: retrieve buffered messages from a running gossip
//! process via IPC.

use clap::{Arg, ArgMatches, Args, Command, FromArgMatches, Parser, value_parser};
use habilis_network::protocol::{MeshId, Nickname};

use super::legacy::LegacyOutput;

/// Who polls: a named session, or whatever a `--state-file` resolves to.
#[derive(Debug)]
pub(crate) enum PollIdentity {
    Session {
        gossip: MeshId,
        nickname: Nickname,
    },
    /// Resolve --gossip/--nickname from a create/join --state-file instead:
    /// wait until that file reports the daemon serving (like `agent-gossip ready`),
    /// then poll as the identity it carries. Lets a poll be armed before the
    /// daemon has minted its identity.
    StateFile(std::path::PathBuf),
}

// Hand-written because clap's derive cannot build one enum from several flags.
// The `Err` arms below are unreachable while the `required_unless_present` and
// `conflicts_with` rules hold; they return an error instead of panicking.
impl Args for PollIdentity {
    fn augment_args(cmd: Command) -> Command {
        cmd.arg(
            Arg::new("gossip")
                .long("gossip")
                .alias("room")
                .value_name("GOSSIP")
                .help("Gossip identifier")
                .value_parser(value_parser!(MeshId))
                .required_unless_present("state_file")
                .conflicts_with("state_file"),
        )
        .arg(
            Arg::new("nickname")
                .long("nickname")
                .value_name("NICKNAME")
                .help("Nickname of the local agent (must have a running join/create session)")
                .value_parser(value_parser!(Nickname))
                .required_unless_present("state_file")
                .conflicts_with("state_file"),
        )
        .arg(
            Arg::new("state_file")
                .long("state-file")
                .value_name("PATH")
                .help(
                    "Resolve --gossip/--nickname from a create/join --state-file instead: \
                     wait until that file reports the daemon serving (like `agent-gossip \
                     ready`), then poll as the identity it carries. Lets a poll be armed \
                     before the daemon has minted its identity.",
                )
                .value_parser(value_parser!(std::path::PathBuf)),
        )
    }

    fn augment_args_for_update(cmd: Command) -> Command {
        Self::augment_args(cmd)
    }
}

impl FromArgMatches for PollIdentity {
    fn from_arg_matches(matches: &ArgMatches) -> Result<Self, clap::Error> {
        Self::from_arg_matches_mut(&mut matches.clone())
    }

    fn from_arg_matches_mut(matches: &mut ArgMatches) -> Result<Self, clap::Error> {
        let missing = |flag: &str| {
            clap::Error::raw(
                clap::error::ErrorKind::MissingRequiredArgument,
                format!("--{flag} is required without --state-file\n"),
            )
        };
        if let Some(path) = matches.remove_one::<std::path::PathBuf>("state_file") {
            return Ok(Self::StateFile(path));
        }
        Ok(Self::Session {
            gossip: matches
                .remove_one("gossip")
                .ok_or_else(|| missing("gossip"))?,
            nickname: matches
                .remove_one("nickname")
                .ok_or_else(|| missing("nickname"))?,
        })
    }

    fn update_from_arg_matches(&mut self, matches: &ArgMatches) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}

#[derive(Parser, Debug)]
pub(crate) struct PollOpts {
    #[command(flatten)]
    pub identity: PollIdentity,

    /// Debug/recovery read: only return events surfaced after this sequence
    /// number, without touching the daemon's read cursor. Hidden — a plain
    /// `poll` tracks the cursor for you (it serves everything not yet served
    /// and remembers where you are).
    #[arg(long, hide = true)]
    pub after: Option<u64>,

    /// Block until an unserved event arrives (long-poll) — the receive bell.
    /// Parks until the daemon holds a waking event it has not yet served to a
    /// plain poll; state/meta document echoes and your own task echoes
    /// (status, artifact, message) never fire it. The daemon holds
    /// each request up to ~60s and the CLI transparently re-issues on an
    /// empty window, so this never times out; a killed call loses nothing.
    /// Omit for an immediate read.
    #[arg(long)]
    pub long: bool,

    /// Wait this many seconds before the bell starts listening. The bell
    /// counts as armed during the wait, which a shell `sleep` before the
    /// command would not.
    #[arg(long, value_name = "SECS", requires = "long")]
    pub settle_secs: Option<u64>,

    /// End the bell by itself once this many seconds have passed: print the
    /// quiet empty batch `[]` and exit 0, as an ordinary empty ring does.
    /// The bell can overshoot by one daemon park (~60s). Use it when a
    /// harness stops a background command at a time limit, so the bell
    /// ends first and the agent re-arms it without a notice.
    #[arg(long, value_name = "SECS", requires = "long")]
    pub max_secs: Option<u64>,

    #[command(flatten)]
    pub legacy_output: LegacyOutput,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::PollIdentity;
    use crate::cli::args::{Cli, Commands};

    fn parse(argv: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(argv)
    }

    #[test]
    fn settle_secs_requires_long() {
        let err = parse(&[
            "agent-gossip",
            "poll",
            "--gossip",
            "g",
            "--nickname",
            "n",
            "--settle-secs",
            "5",
        ])
        .expect_err("--settle-secs without --long must be rejected");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
        let cli = parse(&[
            "agent-gossip",
            "poll",
            "--gossip",
            "g",
            "--nickname",
            "n",
            "--long",
            "--settle-secs",
            "5",
        ])
        .expect("--settle-secs with --long parses");
        let Commands::Poll { opts } = cli.command else {
            panic!("expected Poll command");
        };
        assert_eq!(opts.settle_secs, Some(5));
    }

    #[test]
    fn max_secs_requires_long() {
        let base = ["agent-gossip", "poll", "--gossip", "g", "--nickname", "n"];
        let err = parse(&[&base[..], &["--max-secs", "7000"]].concat())
            .expect_err("--max-secs without --long must be rejected");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
        let cli = parse(&[&base[..], &["--long", "--max-secs", "7000"]].concat())
            .expect("--max-secs with --long parses");
        let Commands::Poll { opts } = cli.command else {
            panic!("expected Poll command");
        };
        assert_eq!(opts.max_secs, Some(7000));
    }

    #[test]
    fn gossip_and_nickname_form_parses() {
        let cli = parse(&[
            "agent-gossip",
            "poll",
            "--gossip",
            "abc",
            "--nickname",
            "calm-fox",
        ])
        .expect("gossip+nickname form must parse");
        let Commands::Poll { opts } = cli.command else {
            panic!("expected poll");
        };
        assert!(matches!(opts.identity, PollIdentity::Session { .. }));
    }

    #[test]
    fn state_file_form_parses_without_identity() {
        let cli = parse(&[
            "agent-gossip",
            "poll",
            "--state-file",
            "/tmp/s.json",
            "--long",
        ])
        .expect("state-file form must parse");
        let Commands::Poll { opts } = cli.command else {
            panic!("expected poll");
        };
        assert!(matches!(opts.identity, PollIdentity::StateFile(_)));
        assert!(opts.long);
    }

    #[test]
    fn state_file_conflicts_with_gossip_and_nickname() {
        assert!(
            parse(&[
                "agent-gossip",
                "poll",
                "--state-file",
                "/tmp/s.json",
                "--gossip",
                "abc",
            ])
            .is_err()
        );
        assert!(
            parse(&[
                "agent-gossip",
                "poll",
                "--state-file",
                "/tmp/s.json",
                "--nickname",
                "calm-fox",
            ])
            .is_err()
        );
    }

    /// `--after` is hidden from help but must keep parsing — it is the
    /// debug/recovery replay and the old-protocol compatibility path.
    #[test]
    fn hidden_after_still_parses() {
        let cli = parse(&[
            "agent-gossip",
            "poll",
            "--gossip",
            "abc",
            "--nickname",
            "calm-fox",
            "--after",
            "41",
        ])
        .expect("hidden flag parses");
        let Commands::Poll { opts } = cli.command else {
            panic!("expected poll");
        };
        assert_eq!(opts.after, Some(41));
    }

    #[test]
    fn identity_is_required_without_state_file() {
        assert!(parse(&["agent-gossip", "poll"]).is_err());
        assert!(parse(&["agent-gossip", "poll", "--gossip", "abc"]).is_err());
        assert!(parse(&["agent-gossip", "poll", "--nickname", "calm-fox"]).is_err());
    }

    #[test]
    fn gossip_without_nickname_is_rejected() {
        let err = parse(&["agent-gossip", "poll", "--gossip", "abc"])
            .expect_err("--gossip alone must be rejected");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn state_file_with_gossip_is_rejected() {
        let err = parse(&[
            "agent-gossip",
            "poll",
            "--state-file",
            "/tmp/s.json",
            "--gossip",
            "abc",
            "--nickname",
            "calm-fox",
        ])
        .expect_err("--state-file together with --gossip must be rejected");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }
}
