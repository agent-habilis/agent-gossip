use clap::Parser;

use super::legacy::LegacyOutput;

use agent_habilis_mesh::protocol::{MeshId, Nickname};

#[derive(Parser, Debug)]
pub(crate) struct LeaveOpts {
    /// The `💬…` id of the gossip to leave — the full id or a unique prefix
    /// of it. Omitted: every gossip owned by the calling session.
    pub gossip: Option<MeshId>,

    /// Leave only the member with this nickname (when this machine hosts
    /// several members of one gossip).
    #[arg(long, requires = "gossip")]
    pub nickname: Option<Nickname>,

    /// The agent-session process that owns the daemons: a daemon is
    /// session-owned when this pid is among its process ancestors.
    /// Defaults to this command's parent process; agent skills pass their
    /// shell's `$PPID` (the agent process itself).
    #[arg(long)]
    pub session_pid: Option<u32>,

    /// Seconds to wait for a signalled daemon's state file to disappear
    /// before reporting it unconfirmed. Hidden — a test knob.
    #[arg(long, hide = true, default_value_t = 5)]
    pub confirm_timeout_secs: u64,

    #[command(flatten)]
    pub legacy_output: LegacyOutput,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands};

    #[test]
    fn leave_defaults() {
        let cli = Cli::parse_from(["agent-gossip", "leave"]);
        let Commands::Leave { opts } = cli.command else {
            panic!("expected Leave command");
        };
        assert!(opts.gossip.is_none());
        assert!(opts.nickname.is_none());
        assert!(opts.session_pid.is_none());
        assert_eq!(opts.confirm_timeout_secs, 5);
    }

    #[test]
    fn leave_accepts_explicit_target() {
        let expected = agent_habilis_mesh::protocol::MeshId::from("AbCdEf1234");
        let cli = Cli::parse_from([
            "agent-gossip",
            "leave",
            expected.as_str(),
            "--nickname",
            "my-nick",
        ]);
        let Commands::Leave { opts } = cli.command else {
            panic!("expected Leave command");
        };
        assert_eq!(opts.gossip.unwrap(), expected);
        assert_eq!(opts.nickname.unwrap().as_str(), "my-nick");
    }

    #[test]
    fn leave_nickname_requires_mesh() {
        assert!(Cli::try_parse_from(["agent-gossip", "leave", "--nickname", "my-nick"]).is_err());
    }
}
