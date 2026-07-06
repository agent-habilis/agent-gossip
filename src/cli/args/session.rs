use clap::Parser;

use super::output::OutputFormat;

#[derive(Parser, Debug)]
pub(crate) struct SessionOpts {
    /// The agent-session process to report for: a daemon counts as this
    /// session's when this pid is among its process ancestors. Defaults to
    /// this command's parent process; agent skills pass their shell's
    /// `$PPID` (the agent process itself).
    #[arg(long)]
    pub session_pid: Option<u32>,

    /// Output format: human (default) or json.
    #[arg(long, default_value = "human")]
    pub output: OutputFormat,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands, OutputFormat};

    #[test]
    fn session_defaults() {
        let cli = Cli::parse_from(["agent-mesh", "session"]);
        let Commands::Session { opts } = cli.command else {
            panic!("expected Session command");
        };
        assert!(opts.session_pid.is_none());
        assert_eq!(opts.output, OutputFormat::Human);
    }

    #[test]
    fn session_accepts_pid_and_json() {
        let cli = Cli::parse_from([
            "agent-mesh",
            "session",
            "--session-pid",
            "42",
            "--output",
            "json",
        ]);
        let Commands::Session { opts } = cli.command else {
            panic!("expected Session command");
        };
        assert_eq!(opts.session_pid, Some(42));
        assert_eq!(opts.output, OutputFormat::Json);
    }
}
