use clap::Parser;

/// The hook protocol `bell-check` answers in: each harness names a refused
/// stop differently.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum StopProtocol {
    /// Claude Code: `{"decision":"block"}`.
    #[default]
    ClaudeCode,
    /// Antigravity: `{"decision":"continue"}`, and it skips the check on a
    /// stop that already followed a refusal.
    Antigravity,
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

    /// The hook protocol to answer in.
    #[arg(long, value_enum, default_value_t)]
    pub protocol: StopProtocol,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::a2a::tuning;
    use crate::cli::args::{Cli, Commands, StopProtocol};

    #[test]
    fn bell_check_parses_session_pid() {
        let cli = Cli::parse_from(["agent-gossip", "bell-check", "--session-pid", "42"]);
        let Commands::BellCheck { options } = cli.command else {
            panic!("expected BellCheck command");
        };
        assert_eq!(
            options.session_pid,
            Some(42),
            "--session-pid 42 must parse to 42"
        );
        assert_eq!(
            options.grace_ms,
            tuning::BELL_CHECK_GRACE_MS,
            "the grace period defaults to the shared tuning constant"
        );
    }

    #[test]
    fn bell_check_protocol_defaults_to_claude_code_and_accepts_antigravity() {
        let parse = |extra: &[&str]| {
            let args = ["agent-gossip", "bell-check"]
                .into_iter()
                .chain(extra.iter().copied());
            let Commands::BellCheck { options } = Cli::parse_from(args).command else {
                panic!("expected BellCheck command");
            };
            options.protocol
        };
        assert_eq!(
            parse(&[]),
            StopProtocol::ClaudeCode,
            "the skills' existing hook passes no protocol and must keep working"
        );
        assert_eq!(
            parse(&["--protocol", "antigravity"]),
            StopProtocol::Antigravity,
            "agy's hooks.json selects its protocol by name"
        );
    }
}
