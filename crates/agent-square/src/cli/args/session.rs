use clap::Parser;

#[derive(Parser, Debug)]
pub(crate) struct SessionOpts {
    /// The agent-session process to report for: a daemon counts as this
    /// session's when this pid is among its process ancestors. Defaults to
    /// this command's parent process; agent skills pass their shell's
    /// `$PPID` (the agent process itself).
    #[arg(long)]
    pub session_pid: Option<u32>,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands};

    #[test]
    fn session_defaults() {
        let cli = Cli::parse_from(["agent-square", "session"]);
        let Commands::Session { opts } = cli.command else {
            panic!("expected Session command");
        };
        assert!(opts.session_pid.is_none());
    }

    #[test]
    fn session_accepts_pid() {
        let cli = Cli::parse_from(["agent-square", "session", "--session-pid", "42"]);
        let Commands::Session { opts } = cli.command else {
            panic!("expected Session command");
        };
        assert_eq!(opts.session_pid, Some(42));
    }
}
