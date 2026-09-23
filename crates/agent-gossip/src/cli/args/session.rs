use clap::Parser;

use super::legacy::LegacyOutput;

#[derive(Parser, Debug)]
pub(crate) struct SessionOpts {
    /// The agent-session process to report for: a daemon counts as this
    /// session's when this pid is among its process ancestors. Defaults to
    /// this command's parent process; agent skills pass their shell's
    /// `$PPID` (the agent process itself).
    #[arg(long)]
    pub session_pid: Option<u32>,

    #[command(flatten)]
    pub legacy_output: LegacyOutput,
}

#[derive(Parser, Debug)]
pub(crate) struct BellCheckOpts {
    /// The agent-session process to check: a daemon counts as this
    /// session's when this pid is among its process ancestors. Defaults to
    /// this command's parent process.
    #[arg(long)]
    pub session_pid: Option<u32>,

    /// How long a missing bell may take to appear before the check fails
    /// (milliseconds). Hidden; tests pass 0.
    #[arg(long, hide = true, default_value_t = crate::a2a::tuning::BELL_CHECK_GRACE_MS)]
    pub grace_ms: u64,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands};

    #[test]
    fn session_defaults() {
        let cli = Cli::parse_from(["agent-gossip", "session"]);
        let Commands::Session { opts } = cli.command else {
            panic!("expected Session command");
        };
        assert!(opts.session_pid.is_none());
    }

    #[test]
    fn bell_check_parses_session_pid() {
        let cli = Cli::parse_from(["agent-gossip", "bell-check", "--session-pid", "42"]);
        let Commands::BellCheck { opts } = cli.command else {
            panic!("expected BellCheck command");
        };
        assert_eq!(opts.session_pid, Some(42));
        assert_eq!(opts.grace_ms, crate::a2a::tuning::BELL_CHECK_GRACE_MS);
    }

    #[test]
    fn session_accepts_pid() {
        let cli = Cli::parse_from(["agent-gossip", "session", "--session-pid", "42"]);
        let Commands::Session { opts } = cli.command else {
            panic!("expected Session command");
        };
        assert_eq!(opts.session_pid, Some(42));
    }
}
