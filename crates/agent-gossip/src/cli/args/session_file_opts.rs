use clap::Parser;

#[derive(Parser, Debug)]
pub(crate) struct SessionFileOpts {
    /// The agent-session process to name the state file for. Skills pass their shell's `$PPID`. Pid 0
    /// and pid 1 are refused, and so is a pid that has no live process.
    #[arg(long)]
    pub session_pid: u32,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands};

    #[test]
    fn session_file_takes_a_pid_and_nothing_else() {
        let cli = Cli::parse_from(["agent-gossip", "session-file", "--session-pid", "42"]);
        let Commands::SessionFile { options } = cli.command else {
            panic!("expected SessionFile command");
        };
        assert_eq!(options.session_pid, 42, "the pid of the agent process");
        assert!(
            Cli::try_parse_from(["agent-gossip", "session-file"]).is_err(),
            "a command that names a file for no process names nothing"
        );
    }
}
