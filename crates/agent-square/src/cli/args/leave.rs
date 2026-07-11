use clap::Parser;

use agent_habilis_mesh::protocol::{MeshId, Nickname};

use super::output::OutputFormat;

#[derive(Parser, Debug)]
pub(crate) struct LeaveOpts {
    /// The `💬…` id of the square to leave — the full id or a unique prefix
    /// of it. Omitted: every square owned by the calling session.
    pub square: Option<MeshId>,

    /// Leave only the member with this nickname (when this machine hosts
    /// several members of one square).
    #[arg(long, requires = "square")]
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

    /// Output format: human (default) or json.
    #[arg(long, default_value = "human")]
    pub output: OutputFormat,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands, OutputFormat};

    #[test]
    fn leave_defaults() {
        let cli = Cli::parse_from(["agent-square", "leave"]);
        let Commands::Leave { opts } = cli.command else {
            panic!("expected Leave command");
        };
        assert!(opts.square.is_none());
        assert!(opts.nickname.is_none());
        assert!(opts.session_pid.is_none());
        assert_eq!(opts.confirm_timeout_secs, 5);
        assert_eq!(opts.output, OutputFormat::Human);
    }

    #[test]
    fn leave_accepts_explicit_target() {
        let cli = Cli::parse_from([
            "agent-square",
            "leave",
            "💬AbCdEf1234",
            "--nickname",
            "my-nick",
            "--output",
            "json",
        ]);
        let Commands::Leave { opts } = cli.command else {
            panic!("expected Leave command");
        };
        assert_eq!(opts.square.unwrap().as_str(), "💬://AbCdEf1234");
        assert_eq!(opts.nickname.unwrap().as_str(), "my-nick");
        assert_eq!(opts.output, OutputFormat::Json);
    }

    #[test]
    fn leave_nickname_requires_mesh() {
        assert!(Cli::try_parse_from(["agent-square", "leave", "--nickname", "my-nick"]).is_err());
    }
}
