use clap::{ArgGroup, Parser};
use habilis_network::protocol::{MeshId, Nickname};

/// The gossip to leave is named once: as GOSSIP, or as `--gossip`, the spelling of `peers`,
/// `topology` and `poll`. The group makes the two conflict and lets `--nickname` follow either.
#[derive(Parser, Debug)]
#[command(group = ArgGroup::new("target").args(["gossip", "gossip_flag"]).multiple(false))]
pub(crate) struct LeaveOpts {
    /// The id of the gossip to leave — the full id or a unique prefix
    /// of it. Omitted: every gossip owned by the calling session.
    pub gossip: Option<MeshId>,

    /// The id of the gossip to leave, as a flag. The same as GOSSIP.
    #[arg(long = "gossip", id = "gossip_flag", value_name = "GOSSIP")]
    pub gossip_flag: Option<MeshId>,

    /// Leave only the member with this nickname (when this machine hosts
    /// several members of one gossip).
    #[arg(long, requires = "target")]
    pub nickname: Option<Nickname>,

    /// The agent-session process that owns the daemons: a daemon is
    /// session-owned when this pid is among its process ancestors.
    /// Defaults to this command's parent process; agent skills pass their
    /// shell's `$PPID` (the agent process itself). Pid 0 and pid 1 are
    /// refused: init owns every daemon.
    #[arg(long)]
    pub session_pid: Option<u32>,
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use habilis_network::util::tuning;

    use crate::cli::args::{Cli, Commands};
    use crate::cli::session::LEAVE_CONFIRM_SECS;

    #[test]
    fn leave_defaults() {
        let cli = Cli::parse_from(["agent-gossip", "leave"]);
        let Commands::Leave { options } = cli.command else {
            panic!("expected Leave command");
        };
        assert!(
            options.gossip.is_none(),
            "leave without --gossip leaves the gossip unset"
        );
        assert!(
            options.nickname.is_none(),
            "leave without --nickname leaves the nickname unset"
        );
        assert!(
            options.session_pid.is_none(),
            "leave without --session-pid leaves the pid unset"
        );
        // A signalled daemon takes up to NODE_LEAVE to close its
        // rendezvous and drop its state file; confirming sooner reports a
        // live daemon as gone, and a relaunch then races it.
        assert!(
            LEAVE_CONFIRM_SECS > tuning::NODE_LEAVE.as_secs(),
            "leave gives up before the daemon's own leave budget: {LEAVE_CONFIRM_SECS}"
        );
    }

    #[test]
    fn leave_accepts_explicit_target() {
        let expected = habilis_network::protocol::MeshId::from("AbCdEf1234");
        let cli = Cli::parse_from([
            "agent-gossip",
            "leave",
            expected.as_str(),
            "--nickname",
            "my-nick",
        ]);
        let Commands::Leave { options } = cli.command else {
            panic!("expected Leave command");
        };
        assert_eq!(
            options.gossip.expect("the test arguments set the gossip"),
            expected,
            "--gossip parses to the id given"
        );
        assert_eq!(
            options
                .nickname
                .expect("the test arguments set the nickname")
                .as_str(),
            "my-nick",
            "--nickname sets the local nickname"
        );
    }

    #[test]
    fn leave_nickname_requires_mesh() {
        assert!(
            Cli::try_parse_from(["agent-gossip", "leave", "--nickname", "my-nick"]).is_err(),
            "a nickname without a gossip is rejected"
        );
    }

    #[test]
    fn leave_takes_the_gossip_as_a_flag_with_a_nickname() {
        let cli = Cli::try_parse_from([
            "agent-gossip",
            "leave",
            "--gossip",
            "AbCdEf1234",
            "--nickname",
            "my-nick",
        ])
        .expect("peers, topology and poll take the gossip as a flag, so leave does too");
        let Commands::Leave { options } = cli.command else {
            panic!("expected Leave command");
        };
        assert_eq!(
            options
                .gossip_flag
                .expect("the test arguments set the flag"),
            habilis_network::protocol::MeshId::from("AbCdEf1234"),
            "the flag carries the id given"
        );
        assert!(
            options.gossip.is_none(),
            "the positional stays unset when the flag names the gossip"
        );
        assert!(
            options.nickname.is_some(),
            "a nickname follows the flag form as it follows the positional"
        );
    }

    #[test]
    fn leave_rejects_the_gossip_named_twice() {
        for args in [
            [
                "agent-gossip",
                "leave",
                "AbCdEf1234",
                "--gossip",
                "AbCdEf1234",
            ],
            [
                "agent-gossip",
                "leave",
                "AbCdEf1234",
                "--gossip",
                "ZzZzZz9876",
            ],
        ] {
            assert!(
                Cli::try_parse_from(args).is_err(),
                "two spellings of one argument leave no rule for which wins: {args:?}"
            );
        }
    }
}
