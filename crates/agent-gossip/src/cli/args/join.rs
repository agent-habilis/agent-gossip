//! `join` command args: attach to an existing gossip by id/domain/repo.

use std::str::FromStr;

use clap::Parser;
use habilis_network::protocol::Nickname;
use habilis_network::protocol::{JoinTarget, JoinTargetError};

use super::shared::SharedServerOpts;
use crate::cli::password::PasswordFlag;

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

/// The positional target as typed. An empty argument names no target, so it
/// parses to `None` and the environment default applies.
#[derive(Clone, Debug)]
pub(crate) struct GossipArg(Option<JoinTarget>);

impl GossipArg {
    pub(crate) fn into_target(self) -> Option<JoinTarget> {
        self.0
    }
}

impl FromStr for GossipArg {
    type Err = String;

    fn from_str(input: &str) -> Result<Self, String> {
        if input.is_empty() {
            return Ok(Self(None));
        }
        parse_join_target(input).map(|target| Self(Some(target)))
    }
}

pub(crate) const DEFAULT_ROOM_ENV: &str = "AGENT_GOSSIP_DEFAULT_ROOM";

/// The argument wins; the environment value fills in only when the argument is
/// absent. An empty environment value counts as unset.
pub(crate) fn resolve_target(
    arg: Option<JoinTarget>,
    env_value: Option<&str>,
) -> Result<JoinTarget, String> {
    if let Some(target) = arg {
        return Ok(target);
    }
    match env_value.filter(|value| !value.is_empty()) {
        Some(value) => parse_join_target(value),
        None => Err(format!(
            "no gossip target. Pass a hash, or set {DEFAULT_ROOM_ENV}."
        )),
    }
}

#[derive(Parser, Debug)]
pub(crate) struct JoinOpts {
    /// Gossip identifier; optional. When omitted or empty, the
    /// `AGENT_GOSSIP_DEFAULT_ROOM` environment variable fills it in. Validated at
    /// parse (clap `FromStr`). For a public gossip derived from a shared string,
    /// use `agent-gossip topic <string>`.
    pub gossip: Option<GossipArg>,

    /// Optional nickname (random word-word if not provided). A custom
    /// nickname is 1..=32 UTF-8 characters, excluding control chars,
    /// whitespace, and any of / \ < > #.
    #[arg(long)]
    pub nickname: Option<Nickname>,

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
    use clap::{CommandFactory, Parser};
    use habilis_network::protocol::{JoinTarget, MeshId};

    use super::{DEFAULT_ROOM_ENV, GossipArg, parse_join_target, resolve_target};
    use crate::cli::args::{Cli, Commands};

    fn hash(seed: &str) -> String {
        MeshId::from(seed).to_string()
    }

    fn parsed_target(args: &[&str]) -> Option<JoinTarget> {
        let Commands::Join { opts } = Cli::try_parse_from(args).unwrap().command else {
            panic!("not a join command");
        };
        opts.gossip.and_then(GossipArg::into_target)
    }

    #[test]
    fn join_without_a_target_parses() {
        assert!(parsed_target(&["agent-gossip", "join"]).is_none());
    }

    #[test]
    fn join_with_an_empty_target_parses_as_no_target() {
        assert!(parsed_target(&["agent-gossip", "join", ""]).is_none());
    }

    #[test]
    fn join_help_names_the_default_room_variable() {
        let help = Cli::command()
            .find_subcommand_mut("join")
            .unwrap()
            .render_long_help()
            .to_string();
        assert!(help.contains(DEFAULT_ROOM_ENV), "got: {help}");
    }

    #[test]
    fn the_argument_wins_over_the_environment() {
        let arg = parse_join_target(&hash("arg")).unwrap();
        let resolved = resolve_target(Some(arg), Some(&hash("env"))).unwrap();
        assert!(matches!(resolved, JoinTarget::Mesh(id) if id.to_string() == hash("arg")));
    }

    #[test]
    fn the_environment_fills_in_a_missing_argument() {
        let resolved = resolve_target(None, Some(&hash("env"))).unwrap();
        assert!(matches!(resolved, JoinTarget::Mesh(id) if id.to_string() == hash("env")));
    }

    #[test]
    fn a_missing_argument_and_environment_names_the_variable() {
        let error = resolve_target(None, None).unwrap_err();
        assert!(error.contains("AGENT_GOSSIP_DEFAULT_ROOM"), "got: {error}");
    }

    #[test]
    fn an_empty_environment_counts_as_unset() {
        let error = resolve_target(None, Some("")).unwrap_err();
        assert!(error.contains("AGENT_GOSSIP_DEFAULT_ROOM"), "got: {error}");
    }

    #[test]
    fn a_bad_environment_value_gets_the_join_error() {
        let error = resolve_target(None, Some("github.com/alice/proj")).unwrap_err();
        assert!(error.contains("agent-gossip topic"), "got: {error}");
    }

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
        let mut mistyped = MeshId::from("join-cli-test").to_string();
        let replacement = if mistyped.ends_with('1') { "2" } else { "1" };
        mistyped.replace_range(mistyped.len() - 1.., replacement);

        let error = Cli::try_parse_from(["agent-gossip", "join", &mistyped]).unwrap_err();
        assert!(error.to_string().contains("invalid gossip hash"));
    }
}
