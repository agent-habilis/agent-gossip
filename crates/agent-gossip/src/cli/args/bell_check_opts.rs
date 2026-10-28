use clap::Parser;

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
    fn bell_check_parses_session_pid() {
        let cli = Cli::parse_from(["agent-gossip", "bell-check", "--session-pid", "42"]);
        let Commands::BellCheck { opts } = cli.command else {
            panic!("expected BellCheck command");
        };
        assert_eq!(opts.session_pid, Some(42));
        assert_eq!(opts.grace_ms, crate::a2a::tuning::BELL_CHECK_GRACE_MS);
    }
}
