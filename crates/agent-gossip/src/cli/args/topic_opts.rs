use clap::Parser;
use habilis_network::protocol::Nickname;

use super::shared_server_opts::SharedServerOpts;

/// Reject an empty / whitespace-only topic string at parse time — it would
/// otherwise hash to a real (but useless, un-guessable-on-purpose) gossip.
fn non_empty_string(raw: &str) -> Result<String, String> {
    if raw.trim().is_empty() {
        return Err("topic string must not be empty".to_owned());
    }
    Ok(raw.to_owned())
}

#[derive(Parser, Debug)]
pub(crate) struct TopicOpts {
    /// Any string — hashed into a deterministic **public** gossip. The same
    /// string always joins the same topic, on any machine, with no id to
    /// share. Compared byte-for-byte after trimming surrounding whitespace, so
    /// `http://…` and `https://…`, or `Repo` and `repo`, are different topics.
    // allow_hyphen_values: "any string" includes ones starting with `-`
    // (e.g. `-release-2026`); without it clap rejects them as unknown flags.
    #[arg(value_parser = non_empty_string, allow_hyphen_values = true)]
    pub string: String,

    /// Optional nickname (random word-word if not provided). A custom
    /// nickname is 1..=32 UTF-8 characters, excluding control chars,
    /// whitespace, and any of / \ < > #.
    #[arg(long)]
    pub nickname: Option<Nickname>,

    #[command(flatten)]
    pub shared: SharedServerOpts,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands};

    fn topic_opts(args: &[&str]) -> super::TopicOpts {
        match Cli::parse_from(args).command {
            Commands::Topic { options } => options,
            Commands::Create { .. }
            | Commands::Join { .. }
            | Commands::Poll { .. }
            | Commands::Ping { .. }
            | Commands::Discover { .. }
            | Commands::Peers { .. }
            | Commands::State { .. }
            | Commands::Meta { .. }
            | Commands::Topology { .. }
            | Commands::A2a { .. }
            | Commands::Ready { .. }
            | Commands::Mcp { .. }
            | Commands::Man
            | Commands::Plug { .. }
            | Commands::Unplug { .. }
            | Commands::Doctor { .. }
            | Commands::Leave { .. }
            | Commands::Invite { .. }
            | Commands::Session { .. }
            | Commands::BellCheck { .. } => panic!("expected Topic command"),
        }
    }

    #[test]
    fn topic_parses_string_and_nickname() {
        let options = topic_opts(&["agent-gossip", "topic", "agent-habilis", "--nickname", "me"]);
        assert_eq!(
            options.string, "agent-habilis",
            "the topic string is kept as given"
        );
        assert_eq!(
            options
                .nickname
                .as_ref()
                .map(habilis_network::protocol::Nickname::as_str),
            Some("me"),
            "--nickname me is kept"
        );
    }

    #[test]
    fn topic_accepts_leading_hyphen_string() {
        let options = topic_opts(&["agent-gossip", "topic", "-release-2026"]);
        assert_eq!(
            options.string, "-release-2026",
            "a leading dash stays part of the topic string"
        );
    }

    // A caller can build `topic <flags> -- <string>` so a string that
    // collides with a known flag still lands in the positional.
    #[test]
    fn topic_accepts_string_after_end_of_flags() {
        let options = topic_opts(&[
            "agent-gossip",
            "topic",
            "--nickname",
            "me",
            "--",
            "--nickname",
        ]);
        assert_eq!(
            options.string, "--nickname",
            "a flag-like topic string is taken as a value"
        );
    }

    #[test]
    fn topic_string_is_required() {
        assert!(
            Cli::try_parse_from(["agent-gossip", "topic"]).is_err(),
            "a topic needs its string"
        );
    }

    #[test]
    fn topic_rejects_empty_string() {
        assert!(
            Cli::try_parse_from(["agent-gossip", "topic", ""]).is_err(),
            "an empty topic string is rejected"
        );
        assert!(
            Cli::try_parse_from(["agent-gossip", "topic", "   "]).is_err(),
            "whitespace-only must reject"
        );
    }
}
