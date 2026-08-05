//! `join` command args: attach to an existing gossip by id/domain/repo.

use clap::Parser;

use crate::cli::password::PasswordFlag;
use fofoca::protocol::Nickname;
use fofoca::protocol::{JoinTarget, JoinTargetError};

use super::shared::SharedServerOpts;

/// Classify a join token, pointing an unrecognized one at `topic` — which is
/// what a plain shared string is for.
///
/// The engine only classifies (`JoinTargetError::Unrecognized`); naming a
/// command, and shell-quoting an argument for one, are this CLI's business. The
/// hint is meant to be copy-pasted, so the string is single-quoted with embedded
/// `'` escaped POSIX-style — unquoted, whitespace would split into extra args
/// and metacharacters could expand.
fn parse_join_target(input: &str) -> Result<JoinTarget, String> {
    input.parse::<JoinTarget>().map_err(|error| match &error {
        JoinTargetError::Unrecognized(token) => {
            let quoted = format!("'{}'", token.replace('\'', "'\\''"));
            format!(
                "{error}. To join a public gossip derived from a shared string, \
                 use `agent-gossip topic {quoted}`."
            )
        }
        JoinTargetError::MalformedMeshId(_) => error.to_string(),
    })
}

#[derive(Parser, Debug)]
pub(crate) struct JoinOpts {
    /// Gossip identifier. Validated at parse (clap `FromStr`). For a
    /// public gossip derived from a shared string, use `agent-gossip topic <string>`.
    #[arg(value_parser = parse_join_target)]
    pub gossip: JoinTarget,

    /// Optional nickname (random word-word if not provided). A custom
    /// nickname is 1..=32 UTF-8 characters, excluding control chars,
    /// whitespace, and any of / \ < > #.
    #[arg(long)]
    pub nickname: Option<Nickname>,

    /// Accepted only to emit a clear error: the network mode is
    /// encoded in the gossip id, so `join` has no `--public`.
    #[arg(long, hide = true)]
    pub public: bool,

    /// Accepted only to emit a clear error: the gossip name is
    /// encoded in the gossip id, so `join` has no `--name`.
    #[arg(long, hide = true)]
    pub name: Option<String>,

    /// Password for a password-protected gossip id — required exactly when
    /// the id carries a password verifier (checked locally before any
    /// network; a wrong password fails immediately). Pass it inline as
    /// `--password=<pw>`; a bare `--password`, or omitting the flag on a
    /// protected id, is an error (there is no terminal prompt).
    #[arg(long, num_args(0..=1), require_equals = true, default_missing_value = "\0")]
    pub password: Option<PasswordFlag>,

    #[command(flatten)]
    pub shared: SharedServerOpts,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::parse_join_target;
    use crate::cli::args::Cli;

    #[test]
    fn a_non_token_string_points_at_topic() {
        let error = parse_join_target("github.com/alice/proj").unwrap_err();
        assert!(
            error.contains("agent-gossip topic 'github.com/alice/proj'"),
            "got: {error}"
        );
    }

    #[test]
    fn the_topic_hint_is_shell_safe() {
        let whitespace = parse_join_target("my secret gossip").unwrap_err();
        assert!(
            whitespace.contains("agent-gossip topic 'my secret gossip'"),
            "got: {whitespace}"
        );
        let quote = parse_join_target("it's here").unwrap_err();
        assert!(
            quote.contains(r"agent-gossip topic 'it'\''s here'"),
            "got: {quote}"
        );
    }

    #[test]
    fn mistyped_gossip_hash_fails_during_cli_parsing() {
        let mut mistyped = fofoca::protocol::MeshId::from("join-cli-test").to_string();
        let replacement = if mistyped.ends_with('1') { "2" } else { "1" };
        mistyped.replace_range(mistyped.len() - 1.., replacement);

        let error = Cli::try_parse_from(["agent-gossip", "join", &mistyped]).unwrap_err();
        assert!(error.to_string().contains("invalid gossip hash"));
    }
}
